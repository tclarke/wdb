mod debugger;
mod disasm;
mod machine;
mod tape;

use debugger::Debugger;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn main() {
    let mut dbg = Debugger::new();

    // Load tape file from command-line argument if provided
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let out = dbg.execute(&format!("load {}", args[1]));
        for line in &out {
            println!("{}", line);
        }
        // Auto-reset to run startup sequence
        let out = dbg.execute("reset");
        for line in &out {
            println!("{}", line);
        }
    }

    println!("WITCH Debugger (type 'help' for commands)");

    let mut rl = DefaultEditor::new().expect("failed to create readline editor");

    loop {
        match rl.readline("(witch) ") {
            Ok(line) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let _ = rl.add_history_entry(line);
                let output = dbg.execute(line);
                for out_line in output {
                    print!("{}", out_line);
                    // add newline if the output doesn't end with one
                    if !out_line.ends_with('\n') {
                        println!();
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("^C");
            }
            Err(ReadlineError::Eof) => {
                break;
            }
            Err(e) => {
                eprintln!("readline error: {}", e);
                break;
            }
        }
    }
}
