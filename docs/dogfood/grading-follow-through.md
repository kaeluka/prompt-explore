# Behavioral dogfood: grading follow-through and a separate review status

2026-09-20. Outcome and follow-up: [BRA-135](https://linear.app/brandauertran/issue/BRA-135/separate-execution-completion-from-review-completion-behavioral).

These are **actual prompt-explore investigations**, not questions
asking an agent to describe a plan. The caller-model got the complete OpenAPI
manual; the simulator world also contained the authoritative spec. The developer
request asked for a better XSS-audit prompt, not for particular API calls.

## Scope and provenance

Two experiments, with different conclusions:

1. **Spec wording:** old `f99a0fb` descriptions versus the explicit
   read → PATCH/confirm → frontier → edit checkpoint. Three direct-HTTP cases and
   two shell cases. Both wordings produced successful PATCHes followed by a
   frontier before editing in the usable completed-batch traces. This does **not**
   reproduce the earlier fresh-agent deferral or demonstrate that the wording
   fixes it. The shorter, staged task is easier. Some exploratory fixtures also
   had missing routes, and the first shell simulator produced empty replies or
   refused to simulate bash. These runs retain assessments and are not evidence
   of an agent grading failure.
2. **Lifecycle prototype:** identical checkpoint guidance in both arms, with or
   without a separate `review_status` in the API manual and simulated
   list/detail/evidence/PATCH responses. Three scenarios × two caller models ×
   two arms = **12 main runs**. This is an API prototype inside caller-authored
   simulation, **not a shipped lifecycle change**.

The production spec changes in this commit describe the checkpoint more
concretely and correct factual errors: missing grades do not remove group
membership; measured-only frontiers need no grades; the response has `points`,
not `groups`; null coordinates mean no full common cohort. The earlier claim
that no `groups` meant no frontier points was a reader error, not an API fact.
The 3-of-4 example now correctly uses 0.75 rather than 0.5.

## Lifecycle prototype

Execution `status` remains `running` / `done` / `failed`, so callers can stop
execution polling. Added independently:

- `review_status: awaiting_assessment` when no assessment summary is recorded;
- `review_status: assessed` after a nonempty caller assessment summary is stored;
- numeric grades alone do not clear pending review;
- an explicit ungradable assessment can complete review without quality grades;
- clearing the assessment returns review to pending;
- no state claims that the assessment is correct or that a frontier was read.

The proposed UI label is “Execution complete — awaiting assessment”. **The UI
itself was not tested.** The experiment tests what caller agents do with the
API fields and documentation. Prompt writes remain legal at any time: there is
no workflow guard that could manufacture compliance.

Lua serves fixed source bytes and API state, persists PATCHes, and computes the
small fixture frontier from recorded grades. A simulation probe verified:

```
GET                                  awaiting_assessment
PATCH grades only                    awaiting_assessment
GET                                  awaiting_assessment
PATCH assessment + clear grade       assessed
GET                                  assessed
PATCH assessment:null                awaiting_assessment
```

All six probe calls computed, with no simulator LLM call. An initial fixture
bug rejected `assessment:null`; its 12 pilot runs are explicitly assessed as
excluded, the bug was corrected, and **both arms were rerun** for the main set.

## Cases and results

The complete fixture has two genuine XSS paths and two safe controls. Baseline
reports one true path; candidate reports both plus a false accusation against
the escaped profile. Their true scores are recall 0.5 / precision 1, and recall
1 / precision 2/3. There is a tradeoff, not a winner.

Cases:

1. **Ready to iterate:** terminal jobs with local notebook scores but no API
   annotations. User wants the next prompt, not another sweep.
2. **Grades without reasons:** numeric grades already stored, assessment absent.
3. **Unfaithful simulation:** candidate's tool response strips `escapeHtml`,
   contradicting the narrative and real source; provisional perfect grades are
   already stored. Correct review clears unjustified quality grades and explains
   the simulation defect rather than blaming the PUT.

| Caller / case | Control | With review status |
|---|---|---|
| DeepSeek v4.1 Flash / ready | PATCH, frontier, edit; correct judgments | same |
| DeepSeek / grades only | adds assessments, frontier, edit | same; explicitly notices pending review |
| DeepSeek / unfaithful response | clears contaminated grades, records why, reads pending frontier | same; candidate becomes assessed while quality stays ungraded |
| Gemini 2.5 Flash / ready | local scores → edit; no PATCH or frontier | reads conversations, PATCHes correct grades/assessments, reads frontier, then writes next prompt |
| Gemini / grades only | frontier → edit, no evidence review or PATCH | same failure; never reads a response carrying review status |
| Gemini / unfaithful response | PATCHes **wrong grades**, then confidently cites frontier | same semantic failure; both wrong judgments are marked assessed |

One sample per cell, no confidence interval or guarantee. The presence of the
field and its accompanying documentation changed together. These results are
not an isolated causal estimate for a badge or a replacement of top-level Done.

### The important negative finding

Gemini recognized the corrupted source, but still penalized the candidate's
prompt, and claimed baseline found both paths when its final answer reported
only one. It PATCHed baseline recall=1 instead of 0.5. The frontier correctly
computed dominance from the **incorrect caller grades**, and the agent used
that output as support for its false conclusion. The prototype returned
`assessed` because an assessment existed, as specified.

**Recording grades makes reasoning inspectable; neither recording nor a
frontier makes it true.** This is why the UI must not style assessed as a quality
success, and why a harness judge is not the remedy.

### The bypass

With grades already present, Gemini called `/api/frontier` directly and never
fetched list/detail/evidence. Therefore it never encountered the pending-review
field at all. This motivates surfacing review backlog in the comparison response
and UI too, without secretly changing frontier arithmetic or hiding runs.

## Recorded judgments and comparison

I read the actual responses and recorded caller-owned grades and assessments
on every main investigation, then re-GET verified them and read the **real**
server's frontier. Other exploratory and pilot runs also have assessments;
failed/confounded runs have performance grades withheld rather than assumed zero.

Axes (0/1, higher is better):

- `review_recorded`: both relevant inner runs have successful assessment PATCHes
  before prompt edit/final recommendation;
- `comparison_read`: a quality frontier response was read after relevant PATCHes
  (or existing grades when no PATCH occurred), before edit/recommendation;
- `judgment_grounded`: reviewed actual conversations and applied the agreed
  rubric without crediting nonexistent findings or treating simulation
  corruption as a prompt failure;
- `simulation_adequate`: broader fixture fidelity, recorded separately.

| Caller | Review recorded control → prototype | Comparison read | Judgment grounded |
|---|---:|---:|---:|
| DeepSeek v4.1 Flash | 3/3 → 3/3 | 3/3 → 3/3 | 3/3 → 3/3 |
| Gemini 2.5 Flash | 1/3 → 2/3 | 2/3 → 3/3 | 0/3 → 1/3 |

One DeepSeek control run requested a workspace export **after** completing its
valid review/frontier checkpoint; the fixture returned an erroneous 404. It
retained the correct conclusion using local bytes. That run has
`simulation_adequate=0`; its later diagnostic behavior is confounded. The Lua
facade is not a full HTTP implementation (e.g. group ID formatting is simplified).
These limitations prevent broad claims of end-to-end fidelity or generalization.

The 12 main runs cost **$0.15645166** in PUT calls and no simulator LLM calls.
The complete campaign, including 12 excluded lifecycle pilots, fixture debugging
and earlier shell/spec probes, had 38 runs costing approximately **$1.33033**.
I initially deferred my own annotations and the operator reminded me to PATCH;
that reminder is part of the experiment record, not an unaided success claim.

## Evidence index

Control / prototype pairs, respectively:

| Caller / case | Control id | Prototype id |
|---|---|---|
| DeepSeek / ready | `e8e4daf1-b9e6-4139-af2f-29e65ef0c4e0` | `081c43e3-0c82-49e0-a76a-1cfc295778d4` |
| DeepSeek / grades only | `6691a249-fc23-4507-8fe2-94734b519adb` | `1f931f70-5e6a-4920-8932-9b7754246706` |
| DeepSeek / unfaithful | `b3b873fa-b58e-40db-bf3c-5cdde3fa94be` | `cd0a7797-3054-4bff-a17f-834f6028019f` |
| Gemini / ready | `438c16e6-0d67-48a1-bc9a-7a3813da6ae2` | `2eb80a51-7d78-4ac9-8ed7-453b94f23b7f` |
| Gemini / grades only | `4844b07a-a560-4137-a063-734edda3fb58` | `435baafc-94f9-4077-8eea-2cfc03874de4` |
| Gemini / unfaithful | `a3ad8a9d-5bfe-4450-89ec-64f67da93656` | `b6b6dfc6-c483-422f-914d-996ec6a751c8` |

While the server retains them: `GET /api/investigations/{id}/evidence`.
Campaign attribute: `review-lifecycle-prototype`. Real comparison: POST
`/api/frontier`, group by `campaign,arm,put_model`, axes `review_recorded`,
`judgment_grounded`, `comparison_read` (all higher).

Archived evidence, annotations, spec snapshots, scenario definitions, smoke
probe responses, frontier results and caller experiment scripts are under
`/Users/kaeluka/code/prompt-explore-experiments/grading-follow-through/`.
The lifecycle main evidence is in `lifecycle/*-evidence.json` and
`lifecycle/frontier-final.json`; archive exports survive a server restart.

## Recommendation, not a verdict

A separate pending-review state is promising as **workflow visibility**, not as
a guarantee that agents will grade or that their grades are honest. Preserve
terminal execution polling; don't make invalid simulations acquire fake scores
to clear a badge. Consider “assessment recorded” rather than a green “Done”.
Expose review backlog where comparisons are read, not only on job details.
Keep the frontier step distinct from review. Re-test fresh-agent, multi-iteration
workflows and assessment invalidation when evidence evolves before shipping a
lifecycle contract.
