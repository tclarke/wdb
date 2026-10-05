# wdb — WITCH Debugger

GDB-style debugger and simulator for the [WITCH computer](https://en.wikipedia.org/wiki/WITCH_computer) (1951, Harwell/Wolverhampton).

## Quick start

```sh
cargo install wdb
wdb program.tape          # load and reset, enter interactive debugger
wdb --batch program.tape  # run non-interactively and exit
# or
wdb                       # blank machine
```

To build locally:

```sh
git clone https://github.com/tclarke/wdb.git
cd web
cargo build --release
./target/release/wdb
```

Prompt: `(witch)`. Type `help` for command list. Ctrl+C during `run` interrupts execution.

## Batch mode

`wdb --batch <tape-file>` loads the tape, resets, runs to completion, and exits. Machine output goes to stdout; load/status messages go to stderr.

Exit codes:

| Code | Meaning                          |
|------|----------------------------------|
| 0    | FINISH (normal halt)             |
| 1    | SIGNAL (alarm halt)              |
| 2    | Overflow                         |
| 3    | Divide by zero                   |
| 4    | Conditional jump without test    |
| 5    | Tape exhausted                   |
| 6    | Tape not loaded                  |
| 7    | Invalid order                    |
| 8    | Invalid address                  |
| 9    | No layout set                    |
| 10   | Same-group violation             |

## Tape format

Compatible with the witchr simulator.

```
tape1(looped)       ; tape <n>(looped|straight), n = 1..7

[1]                 ; block marker 0..9

+1.2345678          ; signed 8-digit fixed-point (+D.DDDDDDD)
+12345678           ; same — decimal after first digit is optional
*12345              ; shorthand: +12345000 (5 digits, padded with 000)

11020               ; 5-digit order: opcode=1 src=10 dst=20
```

- `;` starts a comment (rest of line ignored)
- Whitespace inside numbers is ignored: `1 23 45` = `12345`
- Numbers are fixed-point: decimal after the first digit, range (-10, +10)
- +0 and -0 are distinct values

## Addresses

| Address | As source                               | As destination            |
|---------|-----------------------------------------|---------------------------|
| 00      | round-off value (0 or ±1 in last digit) | drain (discard)           |
| 01–04   | tape reader 1–4 (read next number)      | printer/perforator 1–4    |
| 05–07   | tape reader 5–7                         | spare (discard)           |
| 08      | low 7 digits of accumulator             | write low 7 digits of acc |
| 09      | full accumulator (as 8-digit value)     | write full acc            |
| 10–99   | store                                   | store                     |

## Order set

Format: `OSSDD` — O=opcode, SS=source address, DD=destination address.

| Opcode | Mnemonic    | Operation                        |
|--------|-------------|----------------------------------|
| 1      | ADD-HOLD    | acc += src; dst += acc           |
| 2      | ADD-CLEAR   | acc += src; dst += acc; src = 0  |
| 3      | SUB-HOLD    | acc -= src; dst += acc           |
| 4      | SUB-CLEAR   | acc -= src; dst += acc; src = 0  |
| 5      | MULTIPLY    | acc += src × dst / 10^7; dst = 0 |
| 6      | DIVIDE      | dst = acc / src; acc = remainder |
| 7      | POS-MODULUS | acc += |src|; dst += acc         |
| 0      | CONTROL     | see control orders below         |

Control orders (opcode 0):

| Order | Mnemonic    | Effect                                                |
|-------|-------------|-------------------------------------------------------|
| 00000 | NO-OP       | nothing                                               |
| 00100 | FINISH      | halt (normal)                                         |
| 00200 | SIGNAL      | halt (signal alarm)                                   |
| 011DD | SIGN-TEST+  | next order only if acc > 0, jump to DD otherwise      |
| 012DD | SIGN-TEST-  | next order only if acc < 0, jump to DD otherwise      |
| 021RR | TRANSFER    | move IP to tape reader RR (01–07) or store RR (10–99) |
| 022RR | TRANSFER-IF | conditional transfer (requires prior sign test)       |
| 03BRR | SEARCH      | advance tape RR to block B                            |
| 05BRR | SEARCH-IF   | conditional search                                    |
| 07N00 | SET-LAYOUT  | set output layout N (0–9)                             |
| 08N00 | SET-SHIFT   | one-shot shift A–J for next multiply/divide/add       |

Shift values: 1=A (×10), 2=B (default, ×1), 3=C (×10⁻¹) … 9=J (×10⁻⁷).

Same-group rule: both addresses in an arithmetic order must be in different decade groups (00–09 is one group, 10–19 another, etc.). Violation halts the machine.

## Debugger commands

### Execution

| Command           | Effect                                              |
|-------------------|-----------------------------------------------------|
| `run` / `r`       | Execute until halt or Ctrl+C                        |
| `step` / `s`      | Execute one order                                   |
| `skip`            | Advance tape without executing current order        |
| `exec <order>`    | Execute 5-digit order immediately, don't advance IP |
| `transfer <tape>` | Set IP to tape reader N (equivalent to `021NN`)     |
| `reset`           | Reset machine state, keep tapes loaded              |
| `quit` / `exit`   | Exit the debugger                                   |

### Inspection

| Command            | Effect                                                    |
|--------------------|-----------------------------------------------------------|
| `dis [n]` / `d`    | Disassemble next N orders from current IP (default 1)     |
| `list [tape]` / `l`| Show tape contents from current position                  |
| `print <loc>` / `p`| Print value of location                                   |
| `dump`             | Print full machine state (all stores in grid, acc, flags) |
| `dump tapes`       | Dump all loaded tape contents                             |
| `dump tapes dis`   | Dump all loaded tape contents with disassembly            |

`print` locations: store number (10–99), `acc`, `sign`, `signal`, `alarm`, `layout`, `shift`

### Tape management

| Command                 | Effect                                                    |
|-------------------------|-----------------------------------------------------------|
| `load <file> [tape]`    | Load tape file; optional tape number loads only that tape |
| `clear [tape]`          | Unload tape N, or all tapes if N omitted                  |
| `search <block> [tape]` | Advance tape to block marker N                            |

### Breakpoints

```
break                               list all breakpoints
break block <block> [tape]          add breakpoint at block marker
break line <line> [tape]            add breakpoint at tape line number
break dis <id>                      disable breakpoint
break en <id>                       enable breakpoint
break rm <id>                       remove breakpoint
break when <id> <loc> <op> <value>  add condition to breakpoint
```

Condition operators: `<`, `>`, `=`, `<=`, `>=`, `!=`, `changes`

Condition locations: store number, `acc`, `sign`, `signal`, `alarm`

For `signal`, `sign`, `alarm`: value can be `0/1/on/off/true/false`, or drop op+value to trigger on the indicator lighting.

## Example session

```
$ ./target/debug/wdb program.tape
Loaded tape 1 (42 entries, looped)
Reset. IP at tape 1, block 1.
WITCH Debugger (type 'help' for commands)
(witch) dis 5
tape1+2  [1]        SEARCH      block 1 on reader-1/printer-1
tape1+3  03101      TRANSFER    reader-1/printer-1
tape1+4  02101      ADD-HOLD    reader-1/printer-1 -> store-20
tape1+5  11020      FINISH
tape1+6  00100
(witch) break block 1
Breakpoint 0: tape 1 block 1
(witch) run
Breakpoint 0 hit: tape 1 block 1
(witch) print 20
store-20: +0.0000000
(witch) step
(witch) print 20
store-20: +1.2345678
(witch) dump
Stores:
     0    1    2    3    4    5    6    7    8    9
10  ...
acc: +0.0000000 00000000
sign: off  signal: off  alarm: off
layout: 0  shift: B
```

## Machine behaviour notes

- Overflow (result ≥ 10 or ≤ -10) halts with overflow alarm
- Divide by +0 halts; divide by -0 is permitted
- Conditional transfer without a preceding sign test halts
- Exhausting a straight tape halts; a looped tape wraps silently
- Printer/perforator output appears on stdout
