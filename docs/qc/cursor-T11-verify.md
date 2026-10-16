# T11 fix verification (grok-4.7) — FAIL → fixed by verifier (T11e)
After T11d (14 re-QC findings), verification found:
- [high] wake replay drops the child turn (child events discarded while parent replaying; upstream route held=true).
- [high] Stop misses a background call before its child is attached (upstream childrenOf fallback).
- [medium] finishers leave foreground subagents and child-turn tools Running.
- [medium] reconnect drops every thread's held wakes (upstream sessionsOf scope).
- [medium] background child's late permission dropped after parent turn ends (requestTurn/callsAbove).
- flaky: v2_a_stopped_child_report_settles_interrupted, v2_stop_treats_a_gone_background_child_as_interrupted (events_until silent timeout + Stop race).
Verified fixed: items 2,3,4/13,5,7,9,10; no lock across await.
