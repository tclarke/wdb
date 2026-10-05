; System test: accumulate two tape values through separate stores
;
; 10110  ADD-HOLD src=tape1(01) dst=store10(10): store10 = 0 + 3.0 = +3.0000000
;        (10110 = opcode 1, src=01, dst=10; groups 0 vs 1)
; 10111  ADD-HOLD src=tape1(01) dst=store11(11): store11 = 0 + 4.0 = +4.0000000
;        (10111 = opcode 1, src=01, dst=11; groups 0 vs 1)
; 11020  ADD-HOLD src=store10 dst=store20: store20 = 0 + 3.0 = +3.0000000
; 11120  ADD-HOLD src=store11 dst=store20: store20 = 3.0 + 4.0 = +7.0000000
; 00100  FINISH
;
; expect halt: FINISH
; expect store 10: +3.0000000
; expect store 11: +4.0000000
; expect store 20: +7.0000000

tape1(straight)
[1]
10110           ; store10 += tape1_read  (1 01 10)
+3.0000000      ; 3.0 consumed by 10110
10111           ; store11 += tape1_read  (1 01 11)
+4.0000000      ; 4.0 consumed by 10111
11020           ; store20 += store10
11120           ; store20 += store11
00100           ; FINISH
