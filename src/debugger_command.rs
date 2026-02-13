pub enum DebuggerCommand {
    Quit,
    Run(Vec<String>),
    Continue,
    Backtrace,
    Break(String),
    NaturalBreak(String),
    Next,
    Print(String),
}

impl DebuggerCommand {
    pub fn from_tokens(tokens: &Vec<&str>) -> Option<DebuggerCommand> {
        match tokens[0] {
            "q" | "quit" => Some(DebuggerCommand::Quit),
            "r" | "run" => {
                let args = tokens[1..].to_vec();
                Some(DebuggerCommand::Run(
                    args.iter().map(|s| s.to_string()).collect(),
                ))
            }
            "c" | "cont" | "continue" => Some(DebuggerCommand::Continue),
            "bt" | "back" | "backtrace" => Some(DebuggerCommand::Backtrace),
            "b" | "break" => {
                if tokens.len() < 2 {
                    println!("Usage: b|break <location>");
                    None
                } else {
                    let args = tokens[1..].to_vec();
                    Some(DebuggerCommand::Break(
                        args.iter().map(|s| s.to_string()).collect(),
                    ))
                }
            }
            "n" | "next" => Some(DebuggerCommand::Next),
            "p" | "print" => {
                if tokens.len() < 2 {
                    println!("Usage: p|print <variable>");
                    None
                } else {
                    Some(DebuggerCommand::Print(tokens[1].to_string()))
                }
            }
            "nb" => {
                if tokens.len() < 2 {
                    println!("Usage: nb <自然语言描述>");
                    None
                } else {
                    let description = tokens[1..].join(" ");
                    Some(DebuggerCommand::NaturalBreak(description))
                }
            }
            // Default case:
            _ => None,
        }
    }
}
