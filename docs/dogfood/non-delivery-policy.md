# Agree non-delivery outcomes before experimenting

## Change

The OpenAPI overview now explicitly asks callers to agree the outcome of a
non-delivering attempt **before running the experiment**, along with which
outputs count as delivered. This is caller workflow guidance, not a server gate
or a harness judgment.

Its worked audit rubric distinguishes:

- delivered actionable recall: zero when no findings are delivered, for an
  otherwise gradeable audit with known positive findings;
- precision: undefined with no warnings, rather than automatically zero or one;
- optional usable-report grading, with usability defined by the caller rather
  than inferred from nonempty text;
- genuinely ungradable evidence, which needs an explanatory assessment rather
  than an invented failure score.

The primary frontier example now requests `delivered_recall` and `put_cost_usd`.
Adding undefined precision would remove non-deliveries from that common cohort
again. A separate, labeled conditional precision view avoids that trap. The
text also distinguishes execution-failed jobs, which the frontier excludes even
when graded and which still need explicit failure accounting in recommendations.

Only `info.description` changed in the generated spec. No endpoint shape,
execution/review status, grading rule, or frontier arithmetic changed. Regenerated
`openapi.json` and `API.md`; all 230 tests and `cargo fmt --check` passed.

## Probe design

Four real prompt-explore investigations: two unchanged questions before and after
this description edit, using the same two pinned scenario revisions. The PUT was
`open_router::deepseek/deepseek-v4.1-flash`, thinking `high`, temperature 0,
output allowance 8,000 tokens, one completion step, total PUT budget 100,000 tokens.
Each PUT received the complete corresponding OpenAPI JSON verbatim as its only
product manual (literal template braces escaped only for transport). No tools,
simulator LLM calls, or simulated HTTP behavior were involved.

**These are documentation-comprehension probes, NOT behavioral PATCH/frontier
trials.** The caller explicitly requested proposed requests, not claims that they
were executed. They cannot establish that a future agent will follow through.

Questions:

1. **Before spending:** a developer wants a cheap, useful SQL-injection reviewer.
   Each app has two verified paths and three safe controls. Nothing has run yet;
   help agree the evaluation rules and show the eventual comparison request.
   This question does not itself mention missing answers.
2. **Empty completion:** two completed investigations share a configuration and
   have equivalent apps with two known injections. The first review reports both
   complete paths without false warnings; the second exhausts its tool budget and
   delivers no review. Simulation is faithful, costs are available, and neither
   job has an execution error. Supply proposed PATCH bodies, the primary frontier,
   its expected quality mean, and any additional views. Turn indices are omitted,
   so empty evidence-reference arrays are explicitly permitted in the proposals.

Full inputs, exact spec snapshots, script, evidence and answers:
`/Users/kaeluka/code/prompt-explore-experiments/non-delivery-policy/`.

| Arm | Case | Investigation |
|---|---|---|
| Before | Before spending | `7f6b6f5e-4a17-4ac7-afa4-6d3db1f6b920` |
| Before | Empty completion | `8a06fd8c-b4ad-45da-95ea-1f0778e8c4e1` |
| After | Before spending | `16591ee8-5054-4ae5-82c3-946553af633d` |
| After | Empty completion | `3554facb-340c-435e-8ac9-de6e9b082124` |

## Read findings

### Before spending: no demonstrated improvement

The old-spec answer already discussed non-delivery before spending. It proposed
recall zero and explicitly requested confirmation of an empty-report precision
convention. It recommended precision zero to preserve a joint recall/precision
cohort, rather than retaining undefined precision and separating the views.
Its response ends abruptly after the main comparison request.

The new-spec run returned **empty model_output**, despite `status=done` and
`stop_reason=final_completion`. Both preflight runs reported all 8,000 output
tokens used. The trace contains thinking, but that is not a delivered answer and
is not credited as such. This pair is limited by the output allowance and provides
no evidence that the new wording improves preflight follow-through. Provider raw
finish metadata is unavailable; do not claim a more specific cause than the
observed empty output and allowance usage.

### Empty completion: narrower improvement in the proposed policy

**Before:** proposed recall 1/0, delivery 1/0, and precision **1/1**, explicitly
calling the silent run's precision vacuous. It already computed mean recall 0.5;
there was no failure to count non-delivery on that axis. But it incorrectly said
clearing the silent run's precision would make the *whole group* have null values.
In fact the successful member would still contribute and the point would be
preliminary. It used the invented precision value to keep a four-axis joint cohort.

**After:** proposed `delivered_recall` 1/0 and precision 1/null; requested only
`delivered_recall` and cost in the primary frontier; correctly computed 0.5 across
both attempts. It explained that adding precision would exclude the silent attempt,
and proposed a separate conditional precision view with one contributing run,
explicit exclusions and `preliminary=true`.

This is evidence of clearer proposed precision/cohort handling in one matched
question, **not evidence of an improved PATCH rate or a general reliability fix**.
The after answer still contains an unrelated inconsistency: it labels an optional
view “measured-only” while including the caller-graded `delivered_recall` axis.
Neither answer should be treated as an authoritative manual or executed blindly.

## Recording and limits

The caller read the actual model outputs, recorded assessments on all four real
probe investigations, verified their PATCH echoes, and inspected the frontier
requested with `group_by: [campaign, arm, case]`. Because a frontier considers
all in-memory jobs, it also returned unrelated prior SQL-review null groups,
explicitly excluded for missing this probe-only grade; the four named probe
points each included exactly their one intended investigation. The two no-tool
consultations are not SQL-review evaluations and are separately identified by
campaign/case attributes. Scores describe *delivered documentation guidance*,
never reasoning hidden in thinking.

Total service-reported model cost: **$0.038356668**, with zero simulator cost.
One sample per arm/case, one model, and plans rather than tool execution are
substantial limits. The empty after-preflight response remains part of the
reported result; it is not dropped to make the change look uniformly successful.

The running service was not restarted: its in-memory user investigations remain
intact. The new manual is generated in the repository and takes effect in the
served spec when the updated server is deployed.
