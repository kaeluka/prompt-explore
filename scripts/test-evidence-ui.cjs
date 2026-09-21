/* Browser regression for evidence-first UI. Runs entirely against mock routes:
 * With Playwright installed: node scripts/test-evidence-ui.cjs
 * Or set PLAYWRIGHT_MODULE to an external Playwright installation path.
 */
const http = require('http'), fs = require('fs'), path = require('path');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const root = path.resolve(__dirname, '../server/static');
const token = 'secret-token'; let patches = [], evidenceAuth = false, submitted = [];
const execution = { stop_reason: 'step_budget', steps_used: 3, put_tokens_used: 19, timing: { elapsed_ms: 44, resolving_inputs_ms: 4, put_loop_ms: 32 }, unrendered_call: { name: 'lookup', args: { q: 'never simulated' } } };
const lua = source => ({ path: '.prompt-explore/tools.lua', revisions: [{ source }], setup_workspace_ops: [] });
const implementation = source => [{ tool:'lookup', source_hash:'abc12345', source }];
// ---- Lua workflow orchestration fixtures ----
const wfTurns = [
  { model_output:'planning step', tool_exchanges:[{ call:{name:'lookup',args:{q:'orders'}}, response:{rows:2} }] },
  { model_output:'PLAN TEXT', tool_exchanges:[] },
  { model_output:'VERDICT TEXT', tool_exchanges:[] },
];
const workflowSource = 'return function(args, ctx)\n  local plan = ctx.run_agent({ name = "planner" })\n  local lookup = ctx.call_tool("lookup", { q = args.query })\n  local verify = ctx.run_agent({ name = "verifier", input = plan.output })\n  return { plan = plan.output, lookup = lookup, verdict = verify.output }\nend';
const workflowEvidenceC = {
  source: workflowSource, source_hash:'wfa1b2c3d4e5f6',
  params:{query:'orders'},
  output:{plan:'PLAN TEXT', lookup:{rows:2}, verdict:'PROGRAM OVERRIDE'},
  invocations:[
    { event_id:0, invocation_id:0, index:0, name:'planner', prompt:'You are the planner.', model:'zai_coding::glm-5.2', input:'Plan the task', tools:['lookup','write'], controls:{thinking_level:'low',temperature:0.2,max_tokens:2048}, turn_start:0, turn_end:2, steps_used:2, tokens_used:12, stop_reason:'final_completion', output:'PLAN TEXT' },
    { event_id:2, invocation_id:1, index:1, name:'verifier', prompt:'You are the verifier.', model:'zai_coding::glm-5.2', input:'PLAN TEXT', tools:['lookup'], controls:{}, turn_start:2, turn_end:3, steps_used:1, tokens_used:10, stop_reason:'final_completion', output:'VERDICT TEXT' },
  ],
  tool_calls:[
    { event_id:1, index:0, name:'lookup', args:{q:'orders'}, response:{rows:2}, state_after:{count:2}, lua_execution:{outcome:'computed',tool:'lookup',source_hash:'abc12345'}, workspace_ops:[{tool:'read',args:{path:'x'},result:'y'}] },
  ],
  stop_reason:'final_completion', error:null,
};
const workflowEvidenceD = {
  source:'return function(args, ctx)\n  error("boom")\nend', source_hash:'wfd4e5f6a7b8',
  params:null,
  invocations:[{ index:0, name:'planner', prompt:'You are the planner.', model:'zai_coding::glm-5.2', input:'Plan', tools:['lookup'], controls:{}, turn_start:0, turn_end:1, steps_used:1, tokens_used:5, stop_reason:'runtime_failure', failure:'provider timeout' }],
  tool_calls:[], stop_reason:'runtime_failure', error:'workflow failed: boom',
};
const workflowScenario = { world:'workflow world', input_domain:{}, user_message:'start' };
const jobs = [
  { id:'job-a', status:'done', started_at:Date.now()-1000, finished_at:Date.now()-500, budget:{max_steps_per_trace:3,max_tokens:20}, put:{id:'safe-put',template:'Never cancel without confirmation.',tools:[]}, attributes:{put_model:'alpha',put_thinking:'low',prompt_hash:'a',campaign:'one',simulation_backend:'lua',step_budget:'3',token_budget:'20'}, grades:{}, scenario:{world:'world <img src=x onerror=alert(1)>',input_domain:{},user_message:'hello'}, progress:{}, result:{trace:{execution, turns:[{model_output:'not a final answer',tool_exchanges:[{call:{name:'lookup',args:{q:'<script>bad()</script>'}},response:'<b>response is text</b>',lua_execution:{outcome:'computed',program_revision:0},workspace_ops:[{tool:'read',args:{path:'x'},result:'y'}]}]}],tool_calls:1,implementations:implementation('return { lookup = function() return "a" end }'),simulation_program:lua('return { lookup = function() return "a" end }')},failure:null,usage:null}},
  { id:'job-b', status:'done', started_at:Date.now()-2000, finished_at:Date.now()-1000, budget:{max_steps_per_trace:9,max_tokens:90}, put:{id:'other',tools:[]}, attributes:{put_model:'beta',put_thinking:'high',prompt_hash:'b',campaign:'two',simulation_backend:'llm',step_budget:'9',token_budget:'90'}, grades:{}, scenario:{world:'other',input_domain:{},user_message:'hi'}, progress:{}, result:{trace:{execution:{...execution,stop_reason:'final_completion'},turns:[],tool_calls:0,implementations:implementation('return { lookup = function() return "b" end }'),simulation_program:lua('return { lookup = function() return "b" end }')},failure:null,usage:null}},
  { id:'job-c', status:'done', started_at:Date.now()-3000, finished_at:Date.now()-2000, budget:{max_steps_per_trace:12,max_tokens:120000}, put:{id:'',template:'',design_goals:'',tools:[]}, put_model:'workflow',
    workflow:{lua_source:workflowSource, params:{query:'orders'}, limits:{max_agent_invocations:8,max_direct_tool_calls:64}},
    attributes:{label:'flow-a',workflow_hash:'wfa1b2c3d4e5f6',prompt_hash:'wfa1b2c3d4e5f6',campaign:'three',simulation_backend:'lua',step_budget:'12',token_budget:'120000',scenario_id:'scen-c',scenario_revision:'1',scenario_hash:'shc'}, grades:{}, scenario:workflowScenario, progress:{},
    result:{trace:{execution:{stop_reason:'final_completion',steps_used:5,put_tokens_used:40,timing:{elapsed_ms:120,resolving_inputs_ms:5,orchestration_ms:30,put_loop_ms:85}},turns:wfTurns,resolved_inputs:{query:'orders'},tool_calls:1,implementations:[],workflow:workflowEvidenceC},failure:null,usage:{put:{input_tokens:20,output_tokens:20,cache_read_tokens:0,llm_calls:3,tool_calls:1},sim:{input_tokens:100,output_tokens:20,cache_read_tokens:0,llm_calls:1,tool_calls:0,cost_usd:0.02}}}},
  { id:'job-d', status:'failed', started_at:Date.now()-4000, finished_at:Date.now()-3500, budget:{max_steps_per_trace:12,max_tokens:120000}, put:{id:'',template:'',design_goals:'',tools:[]}, put_model:'workflow',
    workflow:{lua_source:workflowEvidenceD.source, params:null, limits:{}},
    attributes:{label:'flow-fail',workflow_hash:'wfd4e5f6a7b8',prompt_hash:'wfd4e5f6a7b8',campaign:'four'}, grades:{}, scenario:workflowScenario,
    progress:{execution:{stop_reason:'runtime_failure',steps_used:1,put_tokens_used:5,timing:{elapsed_ms:20,resolving_inputs_ms:1,orchestration_ms:4,put_loop_ms:10}},resolved_inputs:{query:'boom'},turns:[{model_output:'',tool_exchanges:[]}],workflow:workflowEvidenceD},
    result:{trace:null, failure:{stage:'workflow', error:'workflow failed: boom'}, usage:null}},
  { id:'job-e', status:'running', started_at:Date.now()-500, finished_at:null, phase:'orchestration', budget:{max_steps_per_trace:12,max_tokens:120000}, put:{id:'',template:'',design_goals:'',tools:[]}, put_model:'workflow',
    workflow:{lua_source:workflowSource, params:{}, limits:{}},
    attributes:{label:'flow-running',workflow_hash:'wfe7f8a9b0c1',prompt_hash:'wfe7f8a9b0c1',campaign:'five'}, grades:{}, scenario:workflowScenario,
    progress:{phase:'orchestration', execution:{steps_used:1,put_tokens_used:3,timing:{elapsed_ms:15,resolving_inputs_ms:2,orchestration_ms:6,put_loop_ms:2}}, resolved_inputs:{query:'live'}, turns:[{model_output:'working',tool_exchanges:[]}], workflow:{source:workflowSource,source_hash:'wfe7f8a9b0c1',params:{},invocations:[{index:0,name:'planner',prompt:'p',model:'zai_coding::glm-5.2',input:'i',tools:[],controls:{},turn_start:0,turn_end:1,steps_used:1,tokens_used:3,stop_reason:'final_completion',output:'partial'}],tool_calls:[],stop_reason:null,error:null}}}
];
function frontier(body) { const by = new Map(); for (const j of jobs) { const a = Object.fromEntries((body.group_by||[]).map(k => [k,Object.prototype.hasOwnProperty.call(j.attributes,k)?j.attributes[k]:null])); const k=JSON.stringify(Object.values(a)); if(!by.has(k))by.set(k,{a,js:[]});by.get(k).js.push(j); } return {points:[...by.values()].map((g,i)=>({id:'g'+i,attributes:g.a,label:Object.values(g.a).join('/'),values:null,on_frontier:null,preliminary:false,dominated_by:[],investigations:g.js.map(j=>j.id),included:[],excluded:[]}))}; }
const server = http.createServer((req,res) => { const u=new URL(req.url,'http://x');
  if(u.pathname==='/api/investigations'&&req.method==='GET') return res.end(JSON.stringify(jobs.map(({id,status,started_at})=>({id,status,started_at}))));
  if(u.pathname==='/api/investigations'&&req.method==='POST'){let s='';req.on('data',x=>s+=x);return req.on('end',()=>{submitted.push(JSON.parse(s));res.setHeader('content-type','application/json');res.end(JSON.stringify({id:'job-new',attributes:{}}));});}
  if(u.pathname==='/api/frontier'&&req.method==='POST'){let s='';req.on('data',x=>s+=x);return req.on('end',()=>{res.setHeader('content-type','application/json');res.end(JSON.stringify(frontier(JSON.parse(s))));});}
  if(u.pathname.match(/^\/api\/investigations\/[^/]+\/evidence$/)){ evidenceAuth=req.headers.authorization===`Bearer ${token}`; if(!evidenceAuth){res.statusCode=401;return res.end('no');}res.setHeader('content-type','application/json');return res.end(JSON.stringify({full:'evidence'})); }
  if(u.pathname.match(/^\/api\/investigations\/[^/]+$/)){const j=jobs.find(x=>x.id===u.pathname.split('/').pop());if(!j){res.statusCode=404;return res.end('not found');}if(req.method==='GET')return res.end(JSON.stringify(j)); if(req.method==='PATCH'){let s='';req.on('data',x=>s+=x);return req.on('end',()=>{const p=JSON.parse(s);patches.push(p);if(Object.prototype.hasOwnProperty.call(p,'assessment'))j.assessment=p.assessment;res.setHeader('content-type','application/json');res.end(JSON.stringify(j));});}}
  const file=u.pathname==='/'?'index.html':u.pathname.slice(1), target=path.resolve(root,file); if(!target.startsWith(root)||!fs.existsSync(target)){res.statusCode=404;return res.end('not found');} if(target.endsWith('.mjs'))res.setHeader('content-type','text/javascript');res.end(fs.readFileSync(target));
});
const ok=(v,m)=>{if(!v)throw Error(m)};
(async()=>{await new Promise(r=>server.listen(0,'127.0.0.1',r));const port=server.address().port, browser=await chromium.launch({headless:true});try { const page=await browser.newPage(); await page.addInitScript(t=>localStorage.setItem('pe_api_token',t),token);
  const state={group_by:['campaign'],axes:[{name:'elapsed_ms',better:'lower'},{name:'put_loop_ms',better:'lower'}],attributes:{campaign:'one'}}; await page.goto(`http://127.0.0.1:${port}/?group_by=${encodeURIComponent(JSON.stringify(state.group_by))}&axes=${encodeURIComponent(JSON.stringify(state.axes))}&attributes=${encodeURIComponent(JSON.stringify(state.attributes))}#keep`); await page.getByRole('heading',{name:'Overview'}).waitFor();
  ok(await page.getByText('Configure comparison',{exact:true}).count()===1,'frontier controls are available behind disclosure'); await page.getByText('Configure comparison',{exact:true}).click(); ok(await page.locator('.axis-grid').first().getByText('lower is better').count()===1,'elapsed_ms is a locked lower-is-better measured axis');
  await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('#job-row-job-a').waitFor();
  ok(await page.locator('#job-row-job-b').count()===0,'URL filter should filter investigation rows'); ok((await page.locator('body').textContent()).includes('investigation list only; all jobs remain frontier candidates'),'filter candidacy warning remains available');
  await page.locator('#job-row-job-a').click(); const card=page.locator('#job-job-a'); await card.getByRole('heading',{name:'safe-put'}).waitFor();
  ok((await card.textContent()).includes('simulation_backend') && (await card.textContent()).includes('step_budget'),'reserved provenance attributes render read-only');
  ok((await card.textContent()).includes('capped (step_budget) — done does not mean a final answer'),'capped execution warning visible'); ok((await card.textContent()).includes('executed, fidelity unverified'),'computed provenance is explicitly unverified'); ok((await card.textContent()).includes('tool request that could not be simulated'),'unrendered tool request is named, not merely implied by counters');
  ok((await card.textContent()).includes('Inputs') && (await card.textContent()).includes('Returned output'),'detail hierarchy starts with inputs and returned output on legacy jobs');
  ok(await card.locator('.investigation-inputs').getByRole('heading',{name:'Single-agent configuration',exact:true}).isVisible(),'legacy configuration is an original input, not inferred from execution');
  ok((await card.locator('.stable-inputs').textContent()).includes('Never cancel without confirmation.'),'legacy configuration retains the original template');
  // XSS payloads are text, never nodes.
  ok(await page.locator('script').filter({hasText:'bad()'}).count()===0,'tool payload did not create a script node'); ok(await page.locator('img').count()===0,'scenario payload did not create an image node');
  await card.getByText('compare tool simulation code with another investigation', {exact:true}).click(); ok(await card.getByLabel('comparison investigation').inputValue()==='job-b','source comparison includes filtered-out investigations'); ok((await card.locator('.source-compare').textContent()).includes('return "b"'),'comparison renders selected other source');
  await card.getByText(/^assessment/).click(); await card.getByLabel('assessment summary').fill('draft survives'); await card.getByLabel('assessment rubric').fill('read traces'); await card.getByLabel('assessment evidence JSON').fill('[{"turn":0,"exchange":0,"note":"tool args"}]');
  // Hide then re-show via filter; only the selected detail is mounted and its draft cache survives.
  await card.getByRole('button',{name:'close detail'}).click(); await page.locator('.filter-row input').fill('no-match'); await page.locator('#jobs').getByText('No investigations match').waitFor(); await page.locator('.filter-row input').fill('one'); await page.locator('#job-row-job-a').click(); if (!await card.getByLabel('assessment summary').isVisible()) await card.getByText(/^assessment/).click(); ok(await card.getByLabel('assessment summary').inputValue()==='draft survives','assessment draft survives filtering');
  await page.getByRole('button',{name:'Overview'}).click(); await page.getByText('Configure comparison',{exact:true}).click(); await page.locator('select[aria-label="grouping attribute 1"]').selectOption('put_model'); await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('#job-row-job-a').click(); if (!await card.getByLabel('assessment summary').isVisible()) await card.getByText(/^assessment/).click(); ok(await card.getByLabel('assessment summary').inputValue()==='draft survives','assessment draft survives regrouping');
  await card.locator('button:has-text("save assessment")').click(); await page.waitForTimeout(20); ok(patches.some(p=>p.assessment&&p.assessment.summary==='draft survives'),'assessment replacement PATCH sent'); await card.locator('button:has-text("clear assessment")').click(); await page.waitForTimeout(20); ok(patches.some(p=>p.assessment===null),'assessment clear PATCH sent');
  const download=page.waitForEvent('download'); await card.getByRole('button',{name:'download evidence JSON'}).click(); await download; ok(evidenceAuth,'evidence download used authenticated fetch'); await card.getByText('raw investigation JSON · complete job API representation',{exact:true}).click(); ok((await card.locator('.raw-json').last().textContent()).includes('"scenario"'),'raw job API representation is available in UI');
  await page.goto(`http://127.0.0.1:${port}/?token=must-not-share&unrelated=keep#hash`); await page.waitForFunction(() => { const q=new URL(location.href).searchParams; const axes=JSON.parse(q.get('axes')||'[]'); return axes.length===2 && axes.every(a=>a.name); });
  const sharedUrl=new URL(page.url()); ok(!sharedUrl.searchParams.has('token'),'share state strips authentication query names'); ok(sharedUrl.searchParams.get('unrelated')==='keep'&&sharedUrl.hash==='#hash','unrelated query/hash preserved');
  const actualAxes=sharedUrl.searchParams.get('axes'); await page.reload(); await page.getByRole('heading',{name:'Overview'}).waitFor(); ok(new URL(page.url()).searchParams.get('axes')===actualAxes,'default axes persist concrete rendered choices');
  await page.goto(`http://127.0.0.1:${port}/?axes=${encodeURIComponent('[{"name":"elapsed_ms","better":"higher"}]')}`); await page.getByText('Invalid shared URL state',{exact:true}).waitFor();

  // ---- Lua workflow orchestration ----
  const wfFilter = v => `attributes=${encodeURIComponent(JSON.stringify({campaign:v}))}`;
  await page.goto(`http://127.0.0.1:${port}/?${wfFilter('three')}`);
  await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('#job-row-job-c').waitFor(); await page.locator('#job-row-job-c').click();
  const wfCard = page.locator('#job-job-c'); await wfCard.getByRole('heading',{name:'flow-a'}).waitFor();
  const wfText = await wfCard.textContent();
  ok((await wfCard.locator('.detail-summary').textContent()).includes('cost unavailable'),'a known simulator price is not shown as the unknown whole-run cost');
  const sectionOrder = ['Inputs', 'Returned output', 'Execution', 'Your assessment', 'Configuration and exports'];
  let lastPos = -1;
  for (const label of sectionOrder) { const pos = wfText.indexOf(label); ok(pos > lastPos, `section order includes ${label}`); lastPos = pos; }
  ok(wfText.includes('User request') && wfText.includes('Resolved scenario inputs'),'scenario definition is input; resolved bindings are execution evidence');
  const inputs = wfCard.locator('.investigation-inputs');
  ok(await inputs.getByRole('heading',{name:'Parameters',exact:true}).isVisible(),'opaque parameters have a generic view');
  ok(!(await inputs.textContent()).includes('You are the planner.'),'runtime prompts are not promoted to investigation inputs');
  const inputsBox = await inputs.boundingBox(), outputBox = await wfCard.locator('.returned-output').boundingBox();
  ok(inputsBox.y + inputsBox.height <= outputBox.y,'stable inputs precede output');
  ok(await inputs.getByText('Lua source',{exact:true}).count()===1,'program source belongs to original inputs');
  const returned = wfCard.locator('.returned-output');
  ok(await returned.isVisible() && (await returned.textContent()).includes('PROGRAM OVERRIDE'),'actual program output is visible without opening any disclosure');
  ok(!(await returned.textContent()).includes('VERDICT TEXT'),'returned output never substitutes the last stage answer');
  const summaries = await wfCard.locator('.call-row').allTextContents();
  ok(summaries[0].includes('planner') && summaries[1].includes('call_tool') && summaries[2].includes('verifier'),'direct calls are interleaved with agents in event_id order');
  ok(await wfCard.locator('.call-detail').count()===1,'only one call detail is mounted');
  const detail=wfCard.locator('.call-detail');
  ok((await detail.locator('.call-arguments').textContent()).includes('You are the planner.'),'selected call shows its actual prompt');
  const headings=await detail.locator('section > h4').allTextContents();
  ok(JSON.stringify(headings)===JSON.stringify(['Arguments','Conversation','Outcome']),'call arguments precede conversation and outcome');
  ok(await detail.getByText('PLAN TEXT',{exact:true}).count()===1,'final stage output is not duplicated');
  await wfCard.locator('.call-row').nth(1).click();
  ok(await wfCard.locator('.call-detail').count()===1 && await detail.locator('.call-conversation').count()===0,'switching to a tool call removes the previous agent conversation');
  ok((await detail.locator('.call-arguments').textContent()).includes('orders') && (await detail.locator('.call-outcome').textContent()).includes('rows'),'direct call arguments and response are retained');
  await wfCard.locator('.call-row').nth(2).click();
  ok((await detail.textContent()).includes('You are the verifier.') && !(await detail.textContent()).includes('You are the planner.'),'selection changes the active call only');
  ok(wfText.includes('run_agent') && wfText.includes('Workflow program'),'submitted workflow program is available with its inputs');
  ok(!(await wfCard.locator('.summary-item .label').allTextContents()).includes('PUT') && (await wfCard.locator('.summary-item .label').allTextContents()).includes('workflow'),'a custom workflow shows a workflow summary, not a misleading single PUT model');
  ok(wfText.includes('orchestration 30ms'),'orchestration timing is shown when recorded');
  await wfCard.getByText('raw investigation JSON · complete job API representation',{exact:true}).click();
  ok((await wfCard.locator('.raw-json').last().textContent()).includes('"workflow"'),'raw investigation JSON remains available for workflow jobs');

  await page.goto(`http://127.0.0.1:${port}/?${wfFilter('four')}`);
  await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('#job-row-job-d').waitFor(); await page.locator('#job-row-job-d').click();
  const failCard = page.locator('#job-job-d'); await failCard.getByRole('heading',{name:'flow-fail'}).waitFor();
  const failText = await failCard.textContent();
  ok(failText.includes('Returned output') && (await failCard.locator('.returned-output').textContent()).includes('Execution failed:') && failText.includes('workflow failed: boom'),'failed workflow shows an explicit failure returned state');
  ok(await failCard.locator('.returned-output').getAttribute('data-output-state')==='failed','failure is not styled as completed output');
  ok((await failCard.locator('.call-outcome').textContent()).includes('provider timeout'),'a failed workflow keeps partial per-call failure evidence');

  await page.goto(`http://127.0.0.1:${port}/?${wfFilter('five')}`);
  await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('#job-row-job-e').waitFor();
  const liveText = await page.locator('#job-row-job-e').textContent();
  ok(liveText.includes('orchestrating workflow'),'the orchestration phase is shown for a live workflow');
  await page.locator('#job-row-job-e').click();
  const liveCard = page.locator('#job-job-e');
  await liveCard.getByRole('heading',{name:'flow-running'}).waitFor();
  const liveTextDetail = await liveCard.textContent();
  ok(liveTextDetail.includes('Returned output') && liveTextDetail.includes('running'),'a live workflow shows an explicit running returned-output state');
  ok(await liveCard.locator('.returned-output').getAttribute('data-output-state')==='running','an intermediate stage answer is not a returned program value');
  ok(!(await liveCard.locator('.returned-output').textContent()).includes('partial'),'intermediate output is absent from the application result');

  // Before an agent starts, arbitrary params must not be guessed to be a prompt.
  const pending = jobs.find(j=>j.id==='job-e');
  pending.progress.workflow.invocations=[]; pending.progress.turns=[];
  pending.workflow.params={prompt:'UNUSED PARAMETER, NOT AN AGENT PROMPT'};
  await page.reload(); await liveCard.locator('.stable-inputs').waitFor();
  ok((await liveCard.locator('.investigation-params').textContent()).includes('UNUSED PARAMETER, NOT AN AGENT PROMPT'),'params are shown generically even when a key is named prompt');
  ok(await liveCard.locator('.call-detail').count()===0 && (await liveCard.locator('.execution-calls').textContent()).includes('No calls yet'),'before calls, only original inputs and an empty execution list exist');

  // A long loop stays one list, not 100 promoted prompts/conversation trees.
  const loopInvocation = i => ({event_id:i,invocation_id:i,name:'repeat',prompt:`runtime prompt ${i+1}`,model:'test::loop',input:`input ${i+1}`,tools:[],controls:{},turn_start:i,turn_end:i+1,steps_used:1,tokens_used:1,stop_reason:'final_completion',output:`output ${i+1}`});
  const loopJob = {id:'job-loop',status:'running',phase:'orchestration',started_at:Date.now(),put:{id:'',template:'',tools:[]},put_model:'workflow',scenario:workflowScenario,budget:{max_steps_per_trace:200,max_tokens:10000},attributes:{label:'100 calls',campaign:'loop'},grades:{},
    workflow:{lua_source:'return function(p,c) for i=1,100 do c.run_agent{prompt="runtime prompt "..i,model=p.model,input="input "..i,name="repeat"} end end',params:{model:'test::loop',prompt:'ordinary parameter',extract:'also ordinary'},limits:{max_agent_invocations:100}},
    progress:{phase:'orchestration',execution:{steps_used:99,put_tokens_used:99,timing:{}},turns:Array.from({length:99},(_,i)=>({model_output:`output ${i+1}`,tool_exchanges:[]})),workflow:{source:'loop',params:{},invocations:Array.from({length:99},(_,i)=>loopInvocation(i)),tool_calls:[]}}};
  jobs.push(loopJob);
  await page.goto(`http://127.0.0.1:${port}/?${wfFilter('loop')}`);
  await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('#job-row-job-loop').click();
  const loopCard=page.locator('#job-job-loop'); await loopCard.locator('.call-row').first().waitFor();
  const stableInputs=await loopCard.locator('.stable-inputs').innerHTML();
  ok(!stableInputs.includes('runtime prompt 1<'),'runtime prompt blocks are absent from original inputs');
  ok(await loopCard.locator('.call-row').count()===99 && await loopCard.locator('.call-detail').count()===1,'99 calls mount one heavy detail');
  await loopCard.locator('.call-row').nth(49).click();
  ok((await loopCard.locator('.call-arguments').textContent()).includes('runtime prompt 50'),'repeated names still identify distinct calls');
  loopJob.progress.workflow.invocations.push({...loopInvocation(99),running:true,stop_reason:null,output:undefined,turn_end:99});
  await page.waitForFunction(()=>document.querySelectorAll('#job-job-loop .call-row').length===100);
  ok(await loopCard.locator('.call-row').nth(49).getAttribute('aria-current')==='true','polling appends calls without moving the selection');
  ok(await loopCard.locator('.stable-inputs').innerHTML()===stableInputs,'investigation inputs stay unchanged during execution');
  ok(await loopCard.locator('.call-detail').count()===1 && await loopCard.locator('.call-conversation .turn').count()===1,'100 calls still render only one selected conversation');
  ok(await loopCard.locator('.call-list').evaluate(el=>el.scrollHeight>el.clientHeight && el.clientHeight<500),'long call list has a bounded scroll viewport');
  await loopCard.locator('.call-row').nth(49).press('End');
  ok((await loopCard.locator('.call-arguments').textContent()).includes('runtime prompt 100'),'keyboard End selects the last call');
  loopJob.progress.turns.push({model_output:'live partial turn',tool_exchanges:[]});
  await loopCard.locator('.call-conversation').getByText('live partial turn',{exact:true}).waitFor();
  ok(await loopCard.locator('.call-detail').count()===1,'selected running call updates in place');
  await page.setViewportSize({width:390,height:844});
  ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth),'nested call browser fits a narrow screen');
  await page.setViewportSize({width:1280,height:720});

  // Submission path: request JSON editor prefilled with a workflow example.
  await page.goto(`http://127.0.0.1:${port}/`);
  await page.getByRole('button',{name:/Investigations/}).click(); await page.locator('.composer > summary').click();
  await page.getByRole('button',{name:'workflow example'}).click();
  await page.getByRole('button',{name:'submit investigation'}).click(); await page.waitForTimeout(40);
  ok(submitted.length===1 && submitted[0].workflow && submitted[0].workflow.lua_source.includes('run_agent'),'composer submits a workflow orchestration request with Lua source verbatim');

  console.log('PASS evidence UI: overview/list/detail disclosure, URL/filter, drafts, assessment PATCH/null, execution evidence, authenticated raw/download, XSS, Lua workflow orchestration');
 } finally {await browser.close();server.close();}})().catch(e=>{console.error(e.stack||e);server.close();process.exitCode=1});
