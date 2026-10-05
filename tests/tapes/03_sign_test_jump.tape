; System test: SIGN-TEST- and conditional SEARCH skip a SIGNAL
;
; 10110  ADD-HOLD src=tape1(01) dst=store10(10): store10 = -1.0000000
;        (10110 = opcode 1, src=01, dst=10)
; 01210  SIGN-TEST- addr=store10: sign_flag = (store10 < 0) = true
;        (01210 = opcode 0, second=1, third=2, dst=10)
; 05201  SEARCH-IF block=2 reader=1: sign_flag=true, advance tape1 past [2]
;        (05201 = opcode 0, second=5, third=2, dst=01)
;        tape scan from current pos: skips Order(00200), finds Block(2), lands on 00100
; 00200  SIGNAL — skipped because search repositioned past it
; [2]
; 00100  FINISH — reached via search
;
; expect halt: FINISH
; expect store 10: -1.0000000

tape1(straight)
[1]
10110           ; store10 += tape1_read  (1 01 10)
-1.0000000      ; value consumed by 10110
01210           ; SIGN-TEST-: sign_flag = store10.is_negative()
05201           ; SEARCH-IF: if sign_flag, search tape1 for block 2
00200           ; SIGNAL (never reached — search jumped past it)
[2]
00100           ; FINISH
