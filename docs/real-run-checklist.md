# Running the newest features once on real GitHub

These features are covered by tests with a stand-in ticket source. Run each once on a **throwaway organization** (not your real one) before you rely on them, and tell the project what you saw.

Setup: a new GitHub organization, the tasklane App installed on it with **All repositories**, the App's **Administration** permission set to write (only for step 4), one model credential, `TASKLANE_SANDBOX=docker`.

| # | Do | You should see | If not |
|---|---|---|---|
| 1 | Connect GitHub, open Repositories | the organization's repositories, each in triage | Connections page; the App must be installed on the organization |
| 2 | Make 3 issues on a repository **before** connecting, connect, press **Import open issues** | 3 tickets with titles appear; the card says "Import finished: 3 new ticket(s)" | Health & setup; the daemon log |
| 3 | Open a pull request by hand into `dev`, press **Review with AI** on the Pull requests page | a verdict, notes and a Big-O line, also as a comment on the pull request | the review compares against the pull request's own base branch |
| 4 | Under Repositories set the repository to full mode, switch on "Merging from here", approve a small pull request into `dev`, press **Merge** | the pull request is merged; a fork pull request or one touching `.github/` is refused with a reason | audit log `pull.merge_refused` |
| 5 | **New project**: plan with the AI, answer, confirm | a private repository with README, `.gitignore`, `.tasklane.yml` and your ticked issues; without the Administration permission the page says what to change | App permissions |
| 6 | Set "Branch pull requests go to" to `dev`, create an issue, approve its plan | a pull request into `dev`; after the merge the issue stays open | the branch must exist |
| 7 | Merge `dev` into `main` with a **merge commit** | within a sync pass (about a minute) the issue gets "Closed: the change … is now in `main`" and closes | a squash merge hides the commit; close by hand |
| 8 | Make an issue whose work already exists | the ticket stops with "Nothing needed changing" and offers "close the issue as done" | |
| 9 | Add a note on the Knowledge page ("answer in Indonesian"), create an issue | the plan is written in that language; the ticket page shows which model ran | |

Record what happened (pass, fail, surprise) in an issue, and remove the throwaway organization afterwards.
