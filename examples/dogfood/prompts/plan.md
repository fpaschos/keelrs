You pick the next issue for a coding agent to implement in this repository (keelrs, a Rust agent
framework). You only look: change no file, branch or issue.

Find the open issues labelled `agent-ready`:

    gh issue list --state open --label agent-ready --json number,title,body,labels

Leave out an issue that is blocked by an open issue (`gh issue view <n> --json number,title,body`
and a "Blocked by" section or link), and one that needs a decision from a person first. Of those
left, pick the one that unblocks the most other work; on a tie, the lowest number.

End by calling `report_plan` once: the issue you picked, or no issue when none is left.
