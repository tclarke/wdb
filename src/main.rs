mod debugger;
mod disasm;
mod machine;
mod tape;

use colored::Colorize;
use debugger::Debugger;
use rustyline::error::ReadlineError;
use rustyline::DefaultEditor;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("wdb {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("wdb {} — WITCH computer debugger", env!("CARGO_PKG_VERSION"));
        println!("Usage: wdb [--batch] [--color|--no-color] [tape-file]");
        println!("  --version, -V  Print version");
        println!("  --help, -h     Print this help");
        println!("  --batch        Run tape non-interactively and exit");
        println!("  --color        Force color output");
        println!("  --no-color     Disable color output");
        println!("Once running, type 'help' for debugger commands.");
        return;
    }

    if args.iter().any(|a| a == "--color") {
        colored::control::set_override(true);
    } else if args.iter().any(|a| a == "--no-color") {
        colored::control::set_override(false);
    }

    let batch = args.iter().any(|a| a == "--batch");

    let mut dbg = Debugger::new();

    // Load tape file from command-line argument if provided
    let tape_arg = args.iter().skip(1).find(|a| !a.starts_with('-'));

    if batch {
        let path = match tape_arg {
            Some(p) => p,
            None => {
                eprintln!("wdb: --batch requires a tape file");
                std::process::exit(1);
            }
        };
        for line in dbg.execute(&format!("load {}", path)) {
            eprintln!("{}", line);
        }
        for line in dbg.execute("reset") {
            eprintln!("{}", line);
        }
        for line in dbg.execute("run") {
            print!("{}", line);
            if !line.ends_with('\n') {
                println!();
            }
        }
        let code = dbg.machine.halt_reason.as_ref().map(|r| r.exit_code()).unwrap_or(0);
        std::process::exit(code);
    }

    if let Some(path) = tape_arg {
        let out = dbg.execute(&format!("load {}", path));
        for line in &out {
            println!("{}", line);
        }
        // Auto-reset to run startup sequence
        let out = dbg.execute("reset");
        for line in &out {
            println!("{}", line);
        }
    }

    println!("{}", "WITCH Debugger (type 'help' for commands)".bold());

    let mut rl = DefaultEditor::new().expect("failed to create readline editor");

    loop {
        match rl.readline("(witch) ") {
            Ok(line) => {
                let line = line.trim();
                if !line.is_empty() {
                    let _ = rl.add_history_entry(line);
                }
                let output = dbg.execute(line);
                let non_empty = output.iter().any(|l| !l.is_empty());
                for out_line in output {
                    print!("{}", out_line);
                    if !out_line.ends_with('\n') {
                        println!();
                    }
                }
                if non_empty {
                    println!();
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
