return function(params, ctx)
  local a = ctx.run_agent {
    name = "baseline",
    prompt = params.system,
    model = params.model,
    input = ctx.input,
    tools = { "list_dir", "read_file", "grep" },
    controls = params.controls or { temperature = 0, max_tokens = 3000 },
  }
  return {
    answer = a.output,
    stop_reason = a.stop_reason,
    steps = a.steps_used,
    tokens = a.tokens_used,
    failure = a.failure,
  }
end