/// Disassemble a 5-digit WITCH order into a human-readable string.
pub fn disassemble(order: u32) -> String {
    let opcode = order / 10000;
    let src = ((order / 100) % 100) as u8;
    let dst = (order % 100) as u8;

    match opcode {
        1 => format!("ADD-HOLD    {} -> {}", addr_name(src), addr_name(dst)),
        2 => format!("ADD-CLEAR   {} -> {}", addr_name(src), addr_name(dst)),
        3 => format!("SUB-HOLD    {} -> {}", addr_name(src), addr_name(dst)),
        4 => format!("SUB-CLEAR   {} -> {}", addr_name(src), addr_name(dst)),
        5 => format!("MULTIPLY    {} × {}", addr_name(src), addr_name(dst)),
        6 => format!("DIVIDE      ACC ÷ {} -> {}", addr_name(src), addr_name(dst)),
        7 => format!("POS-MOD     |{}| -> {}", addr_name(src), addr_name(dst)),
        0 => disasm_control(order, src, dst),
        _ => format!("??? {:05}", order),
    }
}

fn disasm_control(order: u32, _src: u8, dst: u8) -> String {
    let second = (order / 1000) % 10;
    let third = (order / 100) % 10;

    match order {
        0 => "NOP".to_string(),
        100 => "FINISH".to_string(),
        200 => "SIGNAL".to_string(),
        _ if second == 1 && third == 1 => format!("SIGN-TEST+  {}", addr_name(dst)),
        _ if second == 1 && third == 2 => format!("SIGN-TEST-  {}", addr_name(dst)),
        _ if second == 2 && third == 1 => format!("TRANSFER    -> {}", addr_name(dst)),
        _ if second == 2 && third == 2 => format!("TRANSFER-IF -> {}", addr_name(dst)),
        _ if second == 3 => format!("SEARCH      block {} on {}", third, addr_name(dst)),
        _ if second == 5 => format!("SEARCH-IF   block {} on {}", third, addr_name(dst)),
        _ if second == 7 => format!("SET-LAYOUT  {}", third),
        _ if second == 8 => {
            let shift_name = shift_letter(third as u8);
            format!("SET-SHIFT   {} (×{})", shift_name, shift_factor(third as u8))
        }
        _ => format!("??? {:05}", order),
    }
}

fn addr_name(addr: u8) -> String {
    match addr {
        0 => "round-off".to_string(),
        1 => "reader-1/printer-1".to_string(),
        2 => "reader-2/punch-1".to_string(),
        3 => "reader-3/printer-2".to_string(),
        4 => "reader-4/punch-2".to_string(),
        5 => "reader-5".to_string(),
        6 => "reader-6".to_string(),
        7 => "reader-7".to_string(),
        8 => "acc-low7".to_string(),
        9 => "acc".to_string(),
        10..=99 => format!("store-{}", addr),
        _ => format!("addr-{}", addr),
    }
}

fn shift_letter(n: u8) -> &'static str {
    match n {
        1 => "A",
        2 => "B",
        3 => "C",
        4 => "D",
        5 => "E",
        6 => "F",
        7 => "G",
        8 => "H",
        9 => "J",
        _ => "?",
    }
}

fn shift_factor(n: u8) -> String {
    match n {
        1 => "10".to_string(),
        2 => "1".to_string(),
        3 => "0.1".to_string(),
        4 => "0.01".to_string(),
        5 => "0.001".to_string(),
        6 => "0.0001".to_string(),
        7 => "0.00001".to_string(),
        8 => "0.000001".to_string(),
        9 => "0.0000001".to_string(),
        _ => "?".to_string(),
    }
}
