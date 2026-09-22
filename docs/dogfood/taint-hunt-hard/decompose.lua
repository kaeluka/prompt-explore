return function(params, ctx)
  local s1 = ctx.run_agent {
    name = "enumerate",
    prompt = params.enumerate,
    model = params.model,
    input = ctx.input,
    tools = { "list_dir", "read_file", "grep" },
    controls = { temperature = 0, max_tokens = 600 },
  }
  if s1.output == nil then
    return { candidates = nil, answer = nil, stage = 1, failure = s1.failure, steps = s1.steps_used }
  end
  local s2 = ctx.run_agent {
    name = "verify",
    prompt = params.verify,
    model = params.model,
    input = "Candidate flows from stage 1:\n" .. s1.output .. "\n\nVerify each candidate against the repository.",
    tools = { "list_dir", "read_file", "grep" },
    controls = { temperature = 0, max_tokens = 800 },
  }
  return {
    candidates = s1.output,
    answer = s2.output,
    stop_reason = s2.stop_reason,
    stage1_steps = s1.steps_used,
    stage2_steps = s2.steps_used,
    failure = s2.failure,
  }
end