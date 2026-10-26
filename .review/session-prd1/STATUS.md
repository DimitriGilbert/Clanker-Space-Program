# session-prd1 review — in-flight state
## Goal: PRD-informed review — user says "half the game is missing." Every reviewer gets PLAN.md context + evaluates COMPLETENESS vs PRD, not just bugs.
## Done (reports in .review/session-prd1/): 16(glue), 11(VAB), 06+07(progression/RTS), 15(panels), 18(scripting/AI), 23(data content), 13(scene/structures), 12(renderer)
## HEADLINE findings so far: only ~15/40 SimCommands player-reachable; launch bypasses ManufactureCraft; NO RecoverVessel/TerminateVessel path (pad trap); labs/researchers/crew/colonies all sim-complete but ZERO UI; no parachute part → science is transmit-only, loop can't close; RunExperiment drops in space (science_ctx None); no path to OnRails (recover_control dead); contracts 2/6 uncompletable; automation agent-only; VAB root can't move + floor occludes bottom attach + no undo; save/revert absent; tutorial never mentions science.
## REMAINING clusters to review: PRD-01,02,03,04,05,08,09,10,14,17,19,20,21,22,24
## Process: Phase2 reviewers (5 background at a time) → Phase3 verifiers (one per report) → Phase4 fix-plan agent → orchestrate execute, COMMIT+SHIP each phase. User wants builds EACH time.
## User demands: 5 in flight ALWAYS (background not foreground); split big files; every reviewer gets PRD.

## PHASE 3 VERIFICATION — status
## DONE verified (all CONFIRMED, persisted inline in agent transcripts):
##   - verified 16+03: F1 RunExperiment-drops-in-space CONFIRMED; F2 no-path-to-OnRails CONFIRMED (severity CRITICAL); F3 launch-bypass CONFIRMED-but-funds-ARE-charged-at-spawn (no BOM/build-time only); F4 RecoverVessel-unreachable+TerminateVessel-doesnt-exist CONFIRMED; F5 colony/crew-invisible CONFIRMED.
##   - verified 06+23: science loop CONFIRMED pad-playable/space-dead; H1 recover/terminate CONFIRMED; H2 labs/crew dead-3-layers CONFIRMED; H3 colonies-invisible CONFIRMED; H4 launch-bypass+inventory-inert CONFIRMED; H5 2/6 contracts uncompletable CONFIRMED; H6 starter-canisterless+tutorial-hides-science CONFIRMED.
## IN-FLIGHT background verifiers: 11+10(VAB), 18+19+20(automation/server), 15+08+04(panels/save), 05+14+09(orbit/map/phys).
## Remaining unverified (mostly MED/LOW or cross-confirmed): 01,02,12,13,17,21,22,24.
## CRITICAL headline (3 reviewers agreed): NO PATH TO OnRails — player can never reach orbit (no ToRails producer, no auto bubble->rails); maneuver/SOI/transfer/recover all dead. THE top fix.
