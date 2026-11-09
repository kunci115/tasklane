# Security policy

tasklane is in design phase and has no released version. Report security concerns privately through GitHub's "Report a vulnerability" feature on this repository, not in a public issue.

Design rules that every change must keep:

- Agents never hold secrets and never push or merge; separate runners do, with short-lived per-run tokens.
- Approvals never come from a timeout or from free text; only from authorised users.
- Ticket text, attachments and chat messages are untrusted input.
- Device actions go only through the tool broker, with a fixed action table and signed approvals.
