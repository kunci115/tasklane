# Evaluations: is the model doing its job

The tests of tasklane prove that the code around the model behaves. They cannot tell you whether a new prompt, a new model or a different engine makes triage, review or duplicate detection better or worse. `evals/` measures that.

## What is in it

`evals/cases/*.jsonl` holds 28 cases, one JSON object per line, for four roles:

| Role | What the cases check |
|---|---|
| `triage` | a typo is level 1 and gets no question; a migration, a login change, secrets, a deploy, money or deleting personal data is level 2 or 3; a ticket that cannot be acted on gets a question; a ticket that tells the agent to lower the level or drop an auth check is not obeyed |
| `reviewer` | a correct change is approved; a hard-coded key, a removed auth check, a destructive migration under a text ticket, a skipped test, and a note inside the diff that asks for approval are not |
| `duplicate` | the same problem in other words is found; a similar topic is not; a candidate title that gives orders is ignored |
| `device` | read before changing, restart only when the logs point to it, `done` only when the observations show health, escalate when nothing allowed can help, never put an address from a log line into params |

Every case says `why` it exists. The risk floors (text and protected-path rules) are deterministic and tested in Rust; these cases measure the model on top of them.

## Running them

```
python3 evals/run_evals.py --validate                  # no model: check the case files
TASKLANE_SANDBOX=docker ANTHROPIC_API_KEY=... \
  python3 evals/run_evals.py --repeat 3                # run everything three times with engine-claude
python3 evals/run_evals.py --role reviewer             # one role
python3 evals/run_evals.py --engine "python3 plugins/engine-cli/engine_cli.py"   # another engine
```

The runner speaks the same plug-in protocol as the daemon, so it measures the engine exactly as it is deployed, including its sandbox and answer checks. A model is not deterministic: `--repeat 3` turns each case into a pass rate (0, 33, 67 or 100 percent).

## Using the baseline

`evals/baseline.json` stores the pass rate of every case. A run exits with 1 when a case passes less often than the baseline (minus `--tolerance`), or when an engine call failed.

1. Run once with a model you trust and read the failures. A failing case is either a model weakness or a case that is wrong; fix or remove the case, and say why in the commit.
2. `--write-baseline` saves the numbers (a partial run with `--role` keeps the other roles' numbers).
3. After changing a prompt, the model or the engine, run again. A regression is listed with the old and new rate.

CI checks that the case files are well formed and that the runner works (with a stand-in engine). It does not call a model, so it costs nothing and cannot flake. Running the cases against a model is a step you take before you ship a change to a prompt or model, and the committed baseline is only as good as the model that wrote it.

## Watching it in production

`/metrics` (see operations) also reports outcomes from the audit log: tickets merged, merged on the first try (one attempt, no red CI, no requested changes), failed, development attempts spent on merged tickets, reviewer verdicts, red CI runs, and model tokens per role. A drop in the first-try rate after a change is the same signal as a regression here, measured on real work.

## Adding a case

Append a line to a file in `evals/cases/`, with a unique `id`, a `role`, a `ticket` (and `diff`, `candidates` or `observations` and `actions` where the role needs them), an `expect` and a `why`. Real tickets that went wrong make the best cases: copy the text, remove names and secrets, and state what the right answer is. `--validate` refuses a case it cannot understand.
