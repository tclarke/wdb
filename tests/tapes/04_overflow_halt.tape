; System test: arithmetic overflow halts the machine
;
; Load +9.9999999 into store10, add it to store20 twice.
; First add: store20 = 0 + 9.9999999 = +9.9999999 (OK)
; Second add: store20 = 9.9999999 + 9.9999999 = 19.9999998 >= 10 → OVERFLOW
;
; 10110  ADD-HOLD src=tape1(01) dst=store10(10): store10 = +9.9999999
;        (10110 = opcode 1, src=01, dst=10)
; 11020  ADD-HOLD src=store10 dst=store20: store20 = +9.9999999
; 11020  ADD-HOLD src=store10 dst=store20: overflow — store20 + store10 >= 10
; 00100  FINISH (never reached)
;
; expect halt: OVERFLOW: result magnitude >= 10^8 (src=10, dst=20)

tape1(straight)
[1]
10110           ; store10 += tape1_read  (1 01 10)
+9.9999999      ; value consumed by 10110
11020           ; store20 += store10 = +9.9999999
11020           ; store20 += store10 → overflow
00100           ; FINISH (never reached)
