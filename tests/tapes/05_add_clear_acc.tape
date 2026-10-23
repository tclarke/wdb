; System test: add-and-clear with the accumulator as source clears the accumulator
;
; 10109  ACC += tape1_read: ACC = +2.0
; 20910  ADD-CLEAR src=acc(09) dst=store10: store10 = +2.0, ACC = 0
; 10109  ACC += tape1_read: ACC = 0 + 3.0 = +3.0 (would be +5.0 if ACC not cleared)
; 20900  ADD-CLEAR src=acc(09) dst=drain: ACC = 0
; 10109  ACC += tape1_read: ACC = 0 + 1.0
; 20920  ADD-CLEAR src=acc(09) dst=store20: store20 = +1.0
;
; expect halt: FINISH
; expect store 10: +2.0000000
; expect store 20: +1.0000000

tape1(straight)
[1]
10109           ; ACC += tape1_read
+2.0000000
20910           ; store10 += ACC; ACC = 0
10109           ; ACC += tape1_read
+3.0000000
20900           ; drain ACC; ACC = 0
10109           ; ACC += tape1_read
+1.0000000
20920           ; store20 += ACC; ACC = 0
00100           ; FINISH
