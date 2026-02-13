use crate::dwarf_data::DwarfData;

use nix::sys::ptrace;
use nix::sys::signal;
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::mem::size_of;

use std::os::unix::process::CommandExt;
use std::process::Child;
use std::process::Command;

fn align_addr_to_word(addr: usize) -> usize {
    addr & (-(size_of::<usize>() as isize) as usize)
}

use std::collections::HashMap;

#[derive(Clone, Debug)]
pub struct Breakpoint {
    pub addr: usize,
    pub orig_byte: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    /// Indicates inferior stopped. Contains the signal that stopped the process, as well as the
    /// current instruction pointer that it is stopped at.
    Stopped(signal::Signal, usize),

    /// Indicates inferior exited normally. Contains the exit status code.
    Exited(i32),

    /// Indicates the inferior exited due to a signal. Contains the signal that killed the
    /// process.
    Signaled(signal::Signal),
}

/// This function calls ptrace with PTRACE_TRACEME to enable debugging on a process. You should use
/// pre_exec with Command to call this in the child process.
fn child_traceme() -> Result<(), std::io::Error> {
    ptrace::traceme().or(Err(std::io::Error::new(
        std::io::ErrorKind::Other,
        "ptrace TRACEME failed",
    )))
}

pub struct Inferior {
    child: Child,
}

impl Inferior {
    /// Attempts to start a new inferior process. Returns Some(Inferior) if successful, or None if
    /// an error is encountered.
    pub fn new(
        target: &str,
        args: &Vec<String>,
        breakpoints: &mut HashMap<usize, Breakpoint>,
    ) -> Option<Inferior> {
        // TODO: implement me!
        let mut cmd = Command::new(target);
        unsafe {
            cmd.pre_exec(child_traceme);
        }

        let child = cmd.args(args).spawn().ok().unwrap();

        let mut inferior = Inferior { child };

        for (addr, bp) in breakpoints.iter_mut() {
            match inferior.write_byte(*addr, 0xcc) {
                Ok(byte) => bp.orig_byte = byte,
                Err(e) => println!("Error setting breakpoint at {:#x}: {}", addr, e),
            }
        }

        match inferior.wait(None) {
            Ok(Status::Stopped(signal::Signal::SIGTRAP, _)) => Some(inferior),
            _ => None,
        }
    }

    /// Returns the pid of this inferior.
    pub fn pid(&self) -> Pid {
        nix::unistd::Pid::from_raw(self.child.id() as i32)
    }

    /// Calls waitpid on this inferior and returns a Status to indicate the state of the process
    /// after the waitpid call.
    pub fn wait(&self, options: Option<WaitPidFlag>) -> Result<Status, nix::Error> {
        Ok(match waitpid(self.pid(), options)? {
            WaitStatus::Exited(_pid, exit_code) => Status::Exited(exit_code),
            WaitStatus::Signaled(_pid, signal, _core_dumped) => Status::Signaled(signal),
            WaitStatus::Stopped(_pid, signal) => {
                let regs = ptrace::getregs(self.pid())?;
                Status::Stopped(signal, regs.rip as usize)
            }
            other => panic!("waitpid returned unexpected status: {:?}", other),
        })
    }

    pub fn continue_run(&self, signal: Option<signal::Signal>) -> Result<Status, nix::Error> {
        ptrace::cont(self.pid(), signal)?;
        self.wait(None)
    }

    pub fn step(&self) -> Result<Status, nix::Error> {
        ptrace::step(self.pid(), None)?;
        self.wait(None)
    }

    pub fn kill(&mut self) -> Result<(), std::io::Error> {
        self.child.kill()?;
        self.wait(None)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        Ok(())
    }

    pub fn write_byte(&mut self, addr: usize, val: u8) -> Result<u8, nix::Error> {
        let aligned_addr = align_addr_to_word(addr);
        let byte_offset = addr - aligned_addr;
        let word = ptrace::read(self.pid(), aligned_addr as ptrace::AddressType)? as u64;
        let orig_byte = (word >> 8 * byte_offset) & 0xff;
        let masked_word = word & !(0xff << 8 * byte_offset);
        let updated_word = masked_word | ((val as u64) << 8 * byte_offset);
        ptrace::write(
            self.pid(),
            aligned_addr as ptrace::AddressType,
            updated_word as *mut std::ffi::c_void,
        )?;
        Ok(orig_byte as u8)
    }

    pub fn print_backtrace(&self, debug_data: &DwarfData) -> Result<(), nix::Error> {
        let regs = ptrace::getregs(self.pid())?;
        let mut instruction_ptr = regs.rip;
        let mut base_ptr = regs.rbp;
        loop {
            let line_num = debug_data
                .get_line_from_addr(instruction_ptr as usize)
                .unwrap();
            let fun_name = debug_data
                .get_function_from_addr(instruction_ptr as usize)
                .unwrap();
            println!("{}: {}", fun_name, line_num);
            if fun_name == "main" {
                break;
            }
            instruction_ptr =
                ptrace::read(self.pid(), (base_ptr + 8) as ptrace::AddressType)? as u64;
            base_ptr = ptrace::read(self.pid(), base_ptr as ptrace::AddressType)? as u64;
        }

        Ok(())
    }
}

impl Drop for Inferior {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}
