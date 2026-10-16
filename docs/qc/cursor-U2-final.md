# QC: U2 usage + onboarding UI, fix round (Cursor composer-2.5)

Reviewer and fixer: turin-QC-U2-cursor-composer. Two findings:
- The wizard screenshots showed the main chat instead of the wizard.
- The recovery screenshots were blank.

Root cause: the capture harness passed `TURIN_WINID` (`swift path/to/script`) as a single command string to `Command::new`, so `screencapture` never ran. The harness was fixed and the screenshots recaptured in commit fac30e8c, which also merges main.
Gate: `scripts/check.sh -p turin` 2993 passed.

**Verdict: PASS**
