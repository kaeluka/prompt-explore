return function(params, ctx)
  local a = ctx.run_agent {
    name = "baseline",
    prompt = params.system,
    model = params.model,
    input = ctx.input,
    tools = { "list_dir", "read_file", "grep" },
    controls = { temperature = 0, max_tokens = 700 },
  }
  return {
    answer = a.output,
    stop_reason = a.stop_reason,
    steps = a.steps_used,
    failure = a.failure,
  }
end