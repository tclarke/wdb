use std::collections::HashMap;
use std::fs;
use std::path::Path;

use wdb::machine::{HaltReason, Machine, Output};
use wdb::tape::parse_tape_content;

const MAX_STEPS: usize = 100_000;

struct Expectations {
    halt: Option<String>,
    stores: HashMap<u8, String>,
    outputs: Vec<String>,
}

// Parse "; expect ..." annotation lines from raw tape file content.
// Annotations are in comments so the tape parser ignores them.
fn parse_expectations(content: &str) -> Expectations {
    let mut exp = Expectations {
        halt: None,
        stores: HashMap::new(),
        outputs: Vec::new(),
    };

    for line in content.lines() {
        let comment_start = match line.find(';') {
            Some(i) => i,
            None => continue,
        };
        let rest = line[comment_start + 1..].trim();
        let rest = match rest.strip_prefix("expect") {
            Some(r) => r.trim(),
            None => continue,
        };

        if let Some(val) = rest.strip_prefix("halt:") {
            exp.halt = Some(val.trim().to_string());
        } else if let Some(val) = rest.strip_prefix("store") {
            // "store NN: +D.DDDDDDD"
            let val = val.trim();
            if let Some(colon) = val.find(':') {
                if let Ok(addr) = val[..colon].trim().parse::<u8>() {
                    exp.stores.insert(addr, val[colon + 1..].trim().to_string());
                }
            }
        } else if let Some(val) = rest.strip_prefix("output:") {
            exp.outputs.push(val.trim().to_string());
        }
    }

    exp
}

fn run_tape_test(path: &Path) -> Result<(), String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {}", path.display(), e))?;

    let expectations = parse_expectations(&content);

    let tapes = parse_tape_content(&content)
        .map_err(|e| format!("parse error in {}: {}", path.display(), e))?;

    let mut machine = Machine::new();
    for (num, tape) in tapes {
        machine.load_tape(num, tape);
    }
    machine.reset();

    let mut outputs: Vec<Output> = Vec::new();
    let mut halt_reason: Option<HaltReason> = None;

    for _ in 0..MAX_STEPS {
        match machine.step() {
            Ok(out) => outputs.extend(out),
            Err(reason) => {
                halt_reason = Some(reason);
                break;
            }
        }
    }

    let name = path.file_name().unwrap().to_string_lossy();

    let halt = halt_reason
        .ok_or_else(|| format!("[{}] did not halt within {} steps", name, MAX_STEPS))?;

    if let Some(expected) = &expectations.halt {
        let actual = format!("{}", halt);
        if actual != *expected {
            return Err(format!(
                "[{}] halt reason mismatch: got `{}`, expected `{}`",
                name, actual, expected
            ));
        }
    }

    let mut addrs: Vec<u8> = expectations.stores.keys().copied().collect();
    addrs.sort();
    for addr in addrs {
        let expected = &expectations.stores[&addr];
        let actual = machine
            .read_addr(addr)
            .map_err(|e| format!("[{}] read addr {}: {}", name, addr, e))?;
        let actual_str = format!("{}", actual);
        if actual_str != *expected {
            return Err(format!(
                "[{}] store {} mismatch: got `{}`, expected `{}`",
                name, addr, actual_str, expected
            ));
        }
    }

    if !expectations.outputs.is_empty() {
        let actual: Vec<String> = outputs
            .iter()
            .filter_map(|o| match o {
                Output::Print(s) => Some(s.trim().to_string()),
                _ => None,
            })
            .filter(|s| !s.is_empty())
            .collect();
        if actual != expectations.outputs {
            return Err(format!(
                "[{}] output mismatch: got {:?}, expected {:?}",
                name, actual, expectations.outputs
            ));
        }
    }

    Ok(())
}

#[test]
fn system_tests() {
    let tape_dir = Path::new("tests/tapes");
    assert!(tape_dir.exists(), "tests/tapes directory not found");

    let mut paths: Vec<_> = fs::read_dir(tape_dir)
        .expect("cannot read tests/tapes")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("tape"))
        .collect();

    paths.sort();
    assert!(!paths.is_empty(), "no .tape files in tests/tapes");

    let mut failures = Vec::new();
    for path in &paths {
        let name = path.file_stem().unwrap().to_string_lossy();
        match run_tape_test(path) {
            Ok(()) => println!("test {} ... ok", name),
            Err(e) => {
                println!("test {} ... FAILED\n  {}", name, e);
                failures.push(name.into_owned());
            }
        }
    }

    if !failures.is_empty() {
        panic!("failed: {}", failures.join(", "));
    }
}
