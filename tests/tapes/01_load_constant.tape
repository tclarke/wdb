; System test: load a constant from tape into a store
;
; 10110  ADD-HOLD src=tape1(01) dst=store10(10): store10 = 0 + 1.2345678 = +1.2345678
;        (10110 = opcode 1, src=01, dst=10)
; 00100  FINISH
;
; expect halt: FINISH
; expect store 10: +1.2345678

tape1(straight)
[1]
10110           ; ADD-HOLD: store10 += tape1_read  (1 01 10)
+1.2345678      ; value consumed by 11010
00100           ; FINISH
