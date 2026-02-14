use crate::debugger_command::DebuggerCommand;
use crate::dwarf_data::{DwarfData, Error as DwarfError};
use crate::inferior::Inferior;
use crate::inferior::Status;
use nix::sys::signal;
use rustyline::error::ReadlineError;
use rustyline::Editor;
use std::collections::HashMap;
use std::fs;

use crate::inferior::Breakpoint;

pub struct Debugger {
    target: String,
    history_path: String,
    readline: Editor<()>,
    inferior: Option<Inferior>,
    debug_data: DwarfData,
    pub break_point: HashMap<usize, Breakpoint>,
}

impl Debugger {
    /// Initializes the debugger.
    pub fn new(target: &str) -> Debugger {
        // TODO (milestone 3): initialize the DwarfData
        let debug_data = match DwarfData::from_file(target) {
            Ok(val) => {
                val.print();
                val
            }
            Err(DwarfError::ErrorOpeningFile) => {
                println!("Could not open file {}", target);
                std::process::exit(1);
            }
            Err(DwarfError::DwarfFormatError(err)) => {
                println!(
                    "Could not load debugging symbols from {}: {:?}",
                    target, err
                );
                std::process::exit(1);
            }
        };

        let history_path = format!("{}/.deet_history", std::env::var("HOME").unwrap());
        let mut readline = Editor::<()>::new();
        // Attempt to load history from ~/.deet_history if it exists
        let _ = readline.load_history(&history_path);

        Debugger {
            target: target.to_string(),
            history_path,
            readline,
            inferior: None,
            debug_data,
            break_point: HashMap::new(),
        }
    }

    pub fn run(&mut self) {
        loop {
            match self.get_next_command() {
                DebuggerCommand::Run(args) => {
                    if self.inferior.is_some() {
                        println!(
                            "Killing running inferior (pid {})",
                            self.inferior.as_ref().unwrap().pid()
                        );
                        let _ = self.inferior.as_mut().unwrap().kill();
                        self.inferior = None;
                    }
                    if let Some(inferior) =
                        Inferior::new(&self.target, &args, &mut self.break_point)
                    {
                        // Create the inferior
                        self.inferior = Some(inferior);
                        match self.inferior.as_mut().unwrap().continue_run(None) {
                            Ok(Status::Exited(code)) => {
                                println!("Child exited (status {})", code);
                                self.inferior = None;
                            }
                            Ok(Status::Signaled(signal)) => {
                                println!("Child exited (signal {})", signal);
                                self.inferior = None;
                            }
                            Ok(Status::Stopped(signal, rip)) => {
                                println!("Child stopped (signal {})", signal);
                                self.print_stopped_info(rip);
                            }
                            Err(e) => println!("Error continuing inferior: {}", e),
                        }
                    } else {
                        println!("Error starting subprocess");
                    }
                }
                DebuggerCommand::Continue => {
                    if let Some(inferior) = self.inferior.as_mut() {
                        use nix::sys::ptrace;
                        let mut regs = ptrace::getregs(inferior.pid()).unwrap();
                        let rip = regs.rip as usize;
                        let bp_addr = rip - 1;

                        if let Some(bp) = self.break_point.get(&bp_addr) {
                            // We are stopped at a breakpoint. We need to step over it.
                            // 1. Restore original instruction
                            inferior.write_byte(bp_addr, bp.orig_byte).unwrap();
                            // 2. Rewind instruction pointer
                            regs.rip = bp_addr as u64;
                            ptrace::setregs(inferior.pid(), regs).unwrap();
                            // 3. Single step
                            match inferior.step() {
                                Ok(Status::Stopped(signal::Signal::SIGTRAP, _)) => {
                                    // 4. Restore breakpoint
                                    inferior.write_byte(bp_addr, 0xcc).unwrap();
                                }
                                Ok(status) => {
                                    // Child stopped for other reason during step (e.g. exit)
                                    // We should probably handle this, but for now just print status
                                    println!("Child stopped during step (status {:?})", status); // This might not compile if debug is not derived
                                    match status {
                                        Status::Exited(code) => {
                                            println!("Child exited (status {})", code);
                                            self.inferior = None;
                                            continue;
                                        }
                                        Status::Signaled(signal) => {
                                            println!("Child exited (signal {})", signal);
                                            self.inferior = None;
                                            continue;
                                        }
                                        _ => {}
                                    }
                                }
                                Err(e) => {
                                    println!("Error stepping inferior: {}", e);
                                    continue;
                                }
                            }
                        }

                        match inferior.continue_run(None) {
                            Ok(Status::Exited(code)) => {
                                println!("Child exited (status {})", code);
                                self.inferior = None;
                            }
                            Ok(Status::Signaled(signal)) => {
                                println!("Child exited (signal {})", signal);
                                self.inferior = None;
                            }
                            Ok(Status::Stopped(signal, rip)) => {
                                println!("Child stopped (signal {})", signal);
                                self.print_stopped_info(rip);
                            }
                            Err(e) => println!("Error continuing inferior: {}", e),
                        }
                    } else {
                        println!("No inferior to continue");
                    }
                }
                DebuggerCommand::Backtrace => {
                    if let Some(inferior) = self.inferior.as_mut() {
                        match inferior.print_backtrace(&self.debug_data) {
                            Ok(_) => (),
                            Err(e) => println!("Error printing backtrace: {}", e),
                        }
                    } else {
                        println!("No inferior to print backtrace");
                    }
                }
                DebuggerCommand::Break(args) => {
                    let addr = if args.starts_with("*") {
                        // Raw address: break *0x4005b8
                        parse_address(&args[1..])
                    } else if let Ok(line_number) = args.parse::<usize>() {
                        // Line number: break 15
                        self.debug_data.get_addr_for_line(None, line_number)
                    } else {
                        // Function name: break func1
                        self.debug_data.get_addr_for_function(None, &args)
                    };

                    if let Some(addr) = addr {
                        let mut bp = Breakpoint { addr, orig_byte: 0 };
                        self.break_point.insert(addr, bp.clone());
                        println!(
                            "Set breakpoint {} at {:#x}",
                            self.break_point.len() - 1,
                            addr
                        );
                        if let Some(inferior) = self.inferior.as_mut() {
                            match inferior.write_byte(addr, 0xcc) {
                                Ok(orig_byte) => {
                                    bp.orig_byte = orig_byte;
                                    self.break_point.insert(addr, bp);
                                }
                                Err(e) => {
                                    println!("Error setting breakpoint at {:#x}: {}", addr, e)
                                }
                            }
                        }
                    } else {
                        println!("Unable to set breakpoint: {}", args);
                    }
                }
                DebuggerCommand::Next => {
                    if let Some(inferior) = self.inferior.as_mut() {
                        use nix::sys::ptrace;
                        // 获取当前行号（只比较行号数字，不比较地址）
                        let regs = ptrace::getregs(inferior.pid()).unwrap();
                        let current_line_number = self
                            .debug_data
                            .get_line_from_addr(regs.rip as usize)
                            .map(|l| l.number);

                        loop {
                            // 在单步前检查是否停在断点上
                            let mut regs = ptrace::getregs(inferior.pid()).unwrap();
                            let rip = regs.rip as usize;
                            let bp_addr = rip - 1;

                            if let Some(bp) = self.break_point.get(&bp_addr) {
                                // 恢复原始字节、回退 rip、单步、重设断点
                                inferior.write_byte(bp_addr, bp.orig_byte).unwrap();
                                regs.rip = bp_addr as u64;
                                ptrace::setregs(inferior.pid(), regs).unwrap();
                                match inferior.step() {
                                    Ok(Status::Stopped(signal::Signal::SIGTRAP, _)) => {
                                        inferior.write_byte(bp_addr, 0xcc).unwrap();
                                    }
                                    Ok(Status::Exited(code)) => {
                                        println!("Child exited (status {})", code);
                                        self.inferior = None;
                                        break;
                                    }
                                    Ok(Status::Signaled(signal)) => {
                                        println!("Child exited (signal {})", signal);
                                        self.inferior = None;
                                        break;
                                    }
                                    Ok(Status::Stopped(_, rip)) => {
                                        self.print_stopped_info(rip);
                                        break;
                                    }
                                    Err(e) => {
                                        println!("Error stepping inferior: {}", e);
                                        break;
                                    }
                                }
                            } else {
                                // 正常单步
                                match inferior.step() {
                                    Ok(Status::Stopped(_, rip)) => {
                                        let new_line_number = self
                                            .debug_data
                                            .get_line_from_addr(rip)
                                            .map(|l| l.number);
                                        // 如果行号变了（或者从 None 变成了 Some），就停下来
                                        if new_line_number != current_line_number
                                            && new_line_number.is_some()
                                        {
                                            self.print_stopped_info(rip);
                                            break;
                                        }
                                        // 行号没变或者还在无行号区域，继续步进
                                    }
                                    Ok(Status::Exited(code)) => {
                                        println!("Child exited (status {})", code);
                                        self.inferior = None;
                                        break;
                                    }
                                    Ok(Status::Signaled(signal)) => {
                                        println!("Child exited (signal {})", signal);
                                        self.inferior = None;
                                        break;
                                    }
                                    Err(e) => {
                                        println!("Error stepping inferior: {}", e);
                                        break;
                                    }
                                }
                            }
                        }
                    } else {
                        println!("No inferior to step");
                    }
                }
                DebuggerCommand::Print(var_name) => {
                    if let Some(inferior) = self.inferior.as_ref() {
                        use crate::dwarf_data::Location;
                        use nix::sys::ptrace;
                        let regs = ptrace::getregs(inferior.pid()).unwrap();
                        let rip = regs.rip as usize;
                        let rbp = regs.rbp as i64;

                        if let Some(var) = self.debug_data.get_variable_by_name(rip, &var_name) {
                            let addr = match &var.location {
                                Location::Address(a) => *a,
                                Location::FramePointerOffset(offset) => {
                                    // DW_OP_fbreg 基于 CFA，x86-64 上 CFA = rbp + 16
                                    (rbp + 16 + (*offset as i64)) as usize
                                }
                            };
                            match ptrace::read(inferior.pid(), addr as ptrace::AddressType) {
                                Ok(value) => {
                                    let value = value as u64;
                                    let type_name = &var.entity_type.name;
                                    let size = var.entity_type.size;
                                    // 根据大小截断值
                                    let masked = match size {
                                        1 => value & 0xff,
                                        2 => value & 0xffff,
                                        4 => value & 0xffff_ffff,
                                        _ => value,
                                    };
                                    println!("{} = {} ({})", var_name, masked, type_name);
                                }
                                Err(e) => println!("Error reading variable '{}': {}", var_name, e),
                            }
                        } else {
                            println!("Variable '{}' not found in current scope", var_name);
                        }
                    } else {
                        println!("No inferior running");
                    }
                }
                DebuggerCommand::NaturalBreak(description) => {
                    println!("正在解析自然语言断点: \"{}\" ...", description);
                    match crate::llm::parse_with_fallback(&description, &self.debug_data) {
                        Ok(spec) => {
                            let addr = match &spec {
                                crate::llm::BreakpointSpec::Line { file, line } => {
                                    println!(
                                        "LLM 解析结果: 行号断点 (文件: {:?}, 行: {})",
                                        file, line
                                    );
                                    self.debug_data.get_addr_for_line(file.as_deref(), *line)
                                }
                                crate::llm::BreakpointSpec::Function { name } => {
                                    println!("LLM 解析结果: 函数断点 (函数: {})", name);
                                    self.debug_data.get_addr_for_function(None, name)
                                }
                                crate::llm::BreakpointSpec::Address { addr } => {
                                    println!("LLM 解析结果: 地址断点 (地址: {:#x})", addr);
                                    Some(*addr)
                                }
                            };

                            if let Some(addr) = addr {
                                let mut bp = Breakpoint { addr, orig_byte: 0 };
                                self.break_point.insert(addr, bp.clone());
                                println!(
                                    "Set breakpoint {} at {:#x}",
                                    self.break_point.len() - 1,
                                    addr
                                );
                                if let Some(inferior) = self.inferior.as_mut() {
                                    match inferior.write_byte(addr, 0xcc) {
                                        Ok(orig_byte) => {
                                            bp.orig_byte = orig_byte;
                                            self.break_point.insert(addr, bp);
                                        }
                                        Err(e) => {
                                            println!(
                                                "Error setting breakpoint at {:#x}: {}",
                                                addr, e
                                            )
                                        }
                                    }
                                }
                            } else {
                                println!("无法将 LLM 解析结果映射到有效地址: {:?}", spec);
                            }
                        }
                        Err(e) => {
                            println!("自然语言断点解析失败: {}", e);
                        }
                    }
                }
                DebuggerCommand::Quit => {
                    if self.inferior.is_some() {
                        println!(
                            "Killing running inferior (pid {})",
                            self.inferior.as_ref().unwrap().pid()
                        );
                        let _ = self.inferior.as_mut().unwrap().kill();

                        self.inferior = None;
                    }
                    return;
                }
            }
        }
    }

    /// 打印停止时的位置信息和源代码行
    fn print_stopped_info(&self, rip: usize) {
        let line = self.debug_data.get_line_from_addr(rip);
        let function = self.debug_data.get_function_from_addr(rip);
        if let (Some(line), Some(function)) = (&line, function) {
            println!("Stopped at {} {}", function, line);
        } else {
            println!("Stopped at {:#x}", rip);
        }
        // 打印对应的源代码行
        if let Some(line) = &line {
            self.print_source(&line.file, line.number);
        }
    }

    /// 读取源文件并打印指定行号的代码
    fn print_source(&self, file_path: &str, line_number: usize) {
        match fs::read_to_string(file_path) {
            Ok(contents) => {
                let lines: Vec<&str> = contents.lines().collect();
                if line_number >= 1 && line_number <= lines.len() {
                    println!("{:<4} {}", line_number, lines[line_number - 1]);
                }
            }
            Err(_) => {
                // 无法读取源文件，静默跳过
            }
        }
    }

    /// This function prompts the user to enter a command, and continues re-prompting until the user
    /// enters a valid command. It uses DebuggerCommand::from_tokens to do the command parsing.
    ///
    /// You don't need to read, understand, or modify this function.
    fn get_next_command(&mut self) -> DebuggerCommand {
        loop {
            // Print prompt and get next line of user input
            match self.readline.readline("(kdb) ") {
                Err(ReadlineError::Interrupted) => {
                    // User pressed ctrl+c. We're going to ignore it
                    println!("Type \"quit\" to exit");
                }
                Err(ReadlineError::Eof) => {
                    // User pressed ctrl+d, which is the equivalent of "quit" for our purposes
                    return DebuggerCommand::Quit;
                }
                Err(err) => {
                    panic!("Unexpected I/O error: {:?}", err);
                }
                Ok(line) => {
                    if line.trim().len() == 0 {
                        continue;
                    }
                    self.readline.add_history_entry(line.as_str());
                    if let Err(err) = self.readline.save_history(&self.history_path) {
                        println!(
                            "Warning: failed to save history file at {}: {}",
                            self.history_path, err
                        );
                    }
                    let tokens: Vec<&str> = line.split_whitespace().collect();
                    if let Some(cmd) = DebuggerCommand::from_tokens(&tokens) {
                        return cmd;
                    } else {
                        println!("Unrecognized command.");
                    }
                }
            }
        }
    }
}

fn parse_address(addr: &str) -> Option<usize> {
    let addr_without_0x = if addr.to_lowercase().starts_with("0x") {
        &addr[2..]
    } else {
        &addr
    };
    usize::from_str_radix(addr_without_0x, 16).ok()
}
