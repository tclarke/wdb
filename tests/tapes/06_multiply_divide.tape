; System test: multiply and divide results land at the accumulator's 10^-15 scale
;
; 51020  MULTIPLY: ACC += store10 * store20 = 0.5 * 0.25 = +0.125; store20 = 0
; 20930  ADD-CLEAR src=acc dst=store30: store30 = +0.125, ACC = 0
; 64050  DIVIDE: store50 = ACC / store40 = -0.375 / 1.5 = -0.25; ACC = remainder 0
;
; expect halt: FINISH
; expect store 20: +0.0000000
; expect store 30: +0.1250000
; expect store 50: -0.2500000

tape1(straight)
[1]
10110           ; store10 += tape1_read
+0.5000000
10120           ; store20 += tape1_read
+0.2500000
51020           ; ACC += store10 * store20; store20 = 0
20930           ; store30 += ACC; ACC = 0
10109           ; ACC += tape1_read
-0.3750000
10140           ; store40 += tape1_read
+1.5000000
64050           ; store50 = ACC / store40; ACC = remainder
00100           ; FINISH
