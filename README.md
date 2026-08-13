# prompt-explore

Designing agentic prompts is hard - LLMs are notoriously bad at predicting how well a prompt will work.

Being able to quickly try out a prompt is a basic requirements for optimizing a prompt.

This tool executes _any_ agentic prompt (the 'prompt under test', PUT) with _any_ tools that it needs by pairing it up with a second LLM, the simulator. The simulator's job is simple: to mock a response for every tool call.

A user of `prompt-explore` can steer the simulator's behaviour by controlling the world the simulator is operating in.

Example: if you're testing a user support agent prompt, you may want to tell the simulator that the user is a premium subscriber with a long positive shopping history, but the last three shipments were cancelled. The simulator picks that context up and mocks responses accordingly.

The tool is 100% sandboxed, no tool calls can ever reach the outside, no hard drive access to any tools. This makes it easy to run many scenarios in parallel.

## Server architecture

The tool functions as a server with an endpoint that serves a thoroughly documented openapi spec. Simply point your coding agent at 127.0.0.1:8080/openapi.json and it will know how to use this.

## Code

This entire repo is a vibe coded server without auth (only the README is mostly written hand-written). There was no security audit. It comes without warranty.

## Supported APIs

 - AWS Bedrock
 - Vertex API
 - Baseten
 - OpenRouter
 - z.ai coding subscription
 - send a pr or feature request if you'd like more.

## How to try this out

### 1. Build

```
$ cargo build --release
$ target/release/prompt-explore-server --help
prompt-explore-server 0.1.0

Property-based testing for agent behavior. HTTP API + web UI.

USAGE:
    prompt-explore-server [OPTIONS]

OPTIONS:
    --dump-openapi    Print the OpenAPI spec as JSON and exit
    -h, --help        Print this help message and exit

ENVIRONMENT:
    PROMPT_EXPLORE_PROVIDER  Which provider runs the LLM calls (default: zai).
                           zai | zai_standard | openrouter | bedrock | baseten | gemini
    ZAI_API_KEY            API key for zai / zai_standard (coding-plan default).
    OPENROUTER_API_KEY     API key for openrouter.
    bedrock uses the default AWS credential chain (aws sso login, profiles, IMDS).
    gemini uses GCP Application Default Credentials (gcloud auth application-default
                           login). Project: VERTEX_PROJECT_ID or gcloud config;
                           region: VERTEX_LOCATION (default: global).
    BASETEN_API_KEY      API key for baseten (OpenAI-compatible).
    BASETEN_ENDPOINT     Baseten endpoint (default: https://inference.baseten.co/v1/).
    PROMPT_EXPLORE_ADDR    Bind address (default: 0.0.0.0:8080, LAN-reachable).
```

### 2. Setup with your coding agent

Tell your coding agent to
1. read the README. Place an api key for one of the supported providers in a local file and point the agent at the file (or log in using the `aws` or `gcloud` clis).
2. ask the agent to start the server and read the openapi specs.
3. ask the agent to try the tool out, while you watch the output in the web ui at http://127.0.0.1:8080
