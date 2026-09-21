# Lua investigations: implementation smoke test

2026-09-21 · branch `feature/lua-investigations` · experimental, not a release.

## Scope

Test caller-authored composition, not whether splitting a prompt improves quality.
Each investigation runs one Lua program against one pinned scenario. The default
program reads ordinary prompt/model/controls parameters; the legacy single-PUT
request translates to that same program. Agent stages share simulation state but
not conversation history. Direct program tool calls use the same engine.

The HTTP API accepts `workflow` instead of the legacy PUT fields. The UI includes
a request-JSON composer, workflow output, exact stage inputs, chronological event
IDs, direct tool evidence and raw JSON. See [the contract](../workflow-api.md) and
[the two-stage request example](../examples/workflow-extract-review.json).

## Live cases

One closed world: invoice INV-7 totals USD 25; PAY-1 and PAY-2 are both settled
USD 25 payments for that invoice; no refund has happened. Tools read the invoice
and read/write an internal note. All implementations are caller-authored Lua.
There are no sampled variables; each run starts from an empty workspace.

The final simulation probe checked read invoice → write note → read note. All
three calls were computed with no fallback/errors, and the final response contained
the exact written note. This is an actual response/source check, not an inference
from the `computed` label.

All final investigations had an outer 12-step / 10,000-agent-token budget. Model
completions used temperature 0 and at most 500 output tokens. Nano was used for all
agent calls except the second stage of extract/review, which used Mini:
`open_router::openai/gpt-4.1-nano` and `open_router::openai/gpt-4.1-mini`.

| Case | Investigation | Observation |
|---|---|---|
| Legacy single prompt | `2aa9953c-ebd3-4aa0-a510-64d3fbcb95c4` | Default Lua ran one agent and preserved the compatibility request. |
| Default Lua + params | `6065109f-6c54-483a-8f4d-a43d23636239` | Omitted source honored the supplied ordinary parameters. |
| Extract → review | `4c9cdaa7-c7c3-44b1-910b-534f02564442` | Nano output became Mini input byte-for-byte; reviewer had no tools. |
| Direct tools + branch + agent | `51c3a743-62b0-4711-b4d0-adf2f7256055` | Program wrote the note; the later agent read the same exact contents twice. |
| Bounded retry | `1f8ffd81-a583-4a89-b6de-59bce0e09ec3` | First stage stopped at its one-step cap without output; the second fresh stage completed. First-stage evidence/spend remained. |

Five completed runs, seven agent invocations, twelve model completions, nine
actual tool exchanges (including two direct program calls). All final tool replies
were computed, with no simulator-model calls or fallback/errors. Estimated total
model cost was **USD 0.0005259**. This is catalog-based reported-usage accounting,
not a billing guarantee. The iterative documentation probes below cost about
USD 0.1124 across Nano and Mini.

For the retry, attempt 1 consumed 151 tokens. Attempt 2's effective remaining token
cap was 9,849, not a reset 10,000. Its own step allowance was three; the whole run
consumed three steps. Both invocations and their exact inputs remain visible.

## A composition finding, not just a plumbing check

The extractor was asked for payment IDs, amounts, settled status, invoice total
and the refunded flag, without recommendations. Its actual response omitted
PAY-1/PAY-2 and the explicit `refunded=false`, and included recommendations.

The reviewer received exactly that response, not the original tool result. It
could discuss an extra USD 25 payment, but did not receive the missing identifiers
or the explicit refund flag. This is the issue #16 use case: a plausible upstream
answer can discard information needed downstream. The harness makes the loss
inspectable; it does not judge or repair it.

This is **not** evidence that two stages outperform one. There is one run per
variant, prompts/models differ, and no agreed quality rubric was supplied. We
recorded explanatory assessments without numeric quality grades. A measured-only
frontier over output tokens and agent cost contains all five runs, one per variant;
it is not a quality recommendation.

## Problems found and corrected

1. **HTTP runtime lifetime.** The initial implementation created a Tokio runtime
   per workflow. Shared provider HTTP pool connections could be owned by a runtime
   that ended with an earlier investigation; concurrent siblings then failed with
   connection errors. The final implementation runs Lua on a blocking thread but
   polls async work on the caller's long-lived runtime. A deterministic test asserts
   runtime identity across concurrent legacy/default-program runs. The matched
   final batch had no connection failures.
2. **Bad test tool implementation.** The initial write handler omitted
   `state_patch={}`. Its staged write was rolled back, and simulator fallback
   claimed success while a following actual read returned `not found`. We corrected
   the caller-authored handler and re-probed before the final batch. This was a
   fixture correction, not a harness fidelity-enforcement mechanism.
3. **Evidence and bounds.** Review added enforcement of declared agent/direct-call
   caps, runtime gating of hidden tools, per-stage charged cutoff evidence,
   nil absent outputs, effective stage budgets, explicit failed-invocation results,
   live invocation metadata and panic finalization. The old runner and workflow
   now share the same agent-loop implementation. Reaching an exact cap with a final
   completion still records a final completion; attempting more work is refused.
4. **Default-program semantics.** Supplying the default source must not select a
   hidden parameter path. Only the compatibility entry point translates legacy
   request fields; custom/default-program submissions honor their own params.
   The default program propagates an ordinary agent failure rather than silently
   returning no output as a successful application run.

The initial failed attempts remain in the evidence directory. In particular, the
first bounded-loop run recovered from a connection failure, **not** the intended
step cutoff; only the final run demonstrates the step-cap retry. `done` alone
would have hidden these distinctions.

## Spec-only caller probes

Used the same two questions across revisions, passing the complete OpenAPI spec
verbatim as the only manual. Started with GPT-4.1 Nano, and also ran a matched
before/after pair with GPT-4.1 Mini:

1. Submit an extract → review candidate as one investigation, then find its
   output/handoff evidence.
2. Read config through a scenario tool, invoke an agent, and retry up to twice
   without a final output; explain limits and surviving evidence.

Before: the model missed composition, suggested independent runs or invented a
simulated `call_agent` tool, and copied stale inline-scenario/PUT-tools shapes.

Intermediate edits exposed further ambiguity: the model confused workflow Lua
with tool-handler Lua (`ctx.workspace`), assumed orchestration limits delegated
to the simulator, or mixed workflow and legacy PUT fields. We corrected the
endpoint's obsolete example, added explicit two-stage/direct-call examples, and
separated the two Lua contexts and mutually exclusive request forms in the spec.

Results were mixed, not a clean documentation pass. Several after probes used
correct real `ctx.run_agent` stages, `ctx.call_tool`, opaque params and complete
workflow-only request bodies. Later cheap-model replies still omitted common
request fields or incorrectly described the total budget as per-attempt; we
clarified these rules at the request and budget schemas. Mini correctly explained
shared budgets in the final retry probe, but its final composition answer copied
the retry example instead of constructing extract/review. Evidence-navigation
prose also remained inconsistent.

The API contains and validates the relevant contract, and the live requests above
exercise it, but these probes do **not** establish reliable first-reply use by cheap
caller models. All saved probe answers are in `spec-probes/`, including regressions,
not just the favorable iterations. More realistic interactive caller trials are
still warranted; do not mistake the five execution smoke tests for that study.

## Reproduction and artifacts

[Evidence directory](lua-investigations/) contains the final scenario definition,
all five request/evidence pairs, the successful simulation probe, measured-only
frontier, selected initial failure evidence, and spec-probe answers.

Register `scenario.json`'s `definition` as the new POST /api/scenarios `scenario`,
then replace scenario_id in each saved request before submitting. No local Lua
installation or real external tools are required. Provider credentials are needed
only for the agent model calls. Store IDs are local to the test server and will
not survive restart; the committed evidence is the durable record.

Validation: 247 Rust tests passed; browser regression passed, including chronological
stage/direct-call ordering, returned output, failures and request submission. A
live browser check also exercised the two-stage run on desktop and mobile without
JavaScript errors.

## Investigation detail review

The first pass asked for caller assessment before exposing the result, buried the
program's output inside a collapsed conversation, and mixed source/provenance
with the execution story. That was not a good first-time reading order.

Promoting every observed stage prompt into top-level Inputs was also wrong:
it confused original investigation inputs with runtime call arguments, required
placeholders before calls existed, and grew a wall of repeated prompts in loops.

The hierarchy is now **Inputs → Returned output → Execution → Your assessment →
Configuration and exports**. Inputs contains the submitted program, generically
rendered parameters, budget and scenario. No parameter names are interpreted as
prompts. This section stays stable while execution proceeds. Sampled/resolved
scenario bindings belong with execution evidence instead.

Execution is a bounded, chronological call list with ONE selected detail. Repeated
names remain distinct calls, keyed by recorded event/invocation identity. Selecting
a call shows **Arguments → Conversation → Outcome**: an agent's actual prompt
belongs before that agent's conversation, not before the whole investigation.
Appending calls does not steal the selection. Arrow/Home/End navigation works,
and the list/detail layout stacks on narrow screens. Failures, partial turns,
cutoffs, direct tool provenance and raw evidence remain available.

Browser regressions include a 100-call loop fixture (no paid model calls), changing
selection among identically named calls, live appends, stable input rendering,
one mounted conversation, keyboard navigation, and live partial turns. They also
check the original output/assessment/download/XSS behavior. This is an expert and
browser review, not an independent first-time-human usability study.

Still outside this prototype: parallel stages, resume/cancel HTTP endpoints,
external application execution, and program-library management. Lua is cooperatively
bounded in-process execution, not OS process isolation.
