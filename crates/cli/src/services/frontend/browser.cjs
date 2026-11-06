'use strict';
// Only this trusted adapter supplies JavaScript. Callers provide typed selectors
// and assertions, never eval code, browser paths or arbitrary target URLs.
const fs=require('node:fs'),path=require('node:path'),readline=require('node:readline');
const {spawn}=require('node:child_process');
const {createAdapter}=require('./legacy_browser.cjs');
const config=JSON.parse(fs.readFileSync(path.join(__dirname,'browser-input.json')));
const directory=path.join(__dirname,'browser-state');fs.mkdirSync(directory);
const eventFile=path.join(directory,'events.jsonl');fs.writeFileSync(eventFile,'');
const adapter=createAdapter({...config,directory,eventFile,fileArtifacts:true});
let mcp=null,nextId=0,sequence=0;
const pending=new Map();let mcpIdentity=null,tools=null;
function rpc(method,params,notification=false){
  if(notification){mcp.stdin.write(JSON.stringify({jsonrpc:'2.0',method,params})+'\n');return Promise.resolve();}
  const id=++nextId;
  return new Promise((resolve,reject)=>{const timer=setTimeout(()=>{pending.delete(id);reject(Error('DevTools MCP timeout; no retry'));},60000);pending.set(id,{resolve,reject,timer});mcp.stdin.write(JSON.stringify({jsonrpc:'2.0',id,method,params})+'\n');});
}
async function devtools(page,mode){
  if(!config.devtools)throw Error('DevTools not reviewed');
  if(!mcp){
    const entry='/opt/melissa-frontend/node_modules/chrome-devtools-mcp/build/src/bin/chrome-devtools-mcp.js';
    mcp=spawn('/opt/node/bin/node',[entry,'--browser-url=http://127.0.0.1:38299',`--filesystem-root=${directory}`,'--no-usage-statistics','--no-performance-crux','--no-source-maps','--no-javascript-evaluation','--no-file-navigations'],{env:{...process.env,CHROME_DEVTOOLS_MCP_NO_USAGE_STATISTICS:'1'},stdio:['pipe','pipe',fs.openSync(path.join(directory,'devtools.log'),'w')]});
    readline.createInterface({input:mcp.stdout}).on('line',line=>{try{const value=JSON.parse(line),slot=pending.get(value.id);if(slot){pending.delete(value.id);clearTimeout(slot.timer);value.error?slot.reject(Error(JSON.stringify(value.error))):slot.resolve(value.result);}}catch{for(const slot of pending.values()){clearTimeout(slot.timer);slot.reject(Error('Invalid MCP frame'));}pending.clear();}});
    mcp.on('exit',()=>{for(const slot of pending.values()){clearTimeout(slot.timer);slot.reject(Error('MCP stopped'));}pending.clear();});
    mcpIdentity=await rpc('initialize',{protocolVersion:'2025-11-25',capabilities:{},clientInfo:{name:'melissa-owned-browser',version:'1'}});
    if(mcpIdentity.protocolVersion!=='2025-11-25')throw Error('MCP protocol mismatch');
    await rpc('notifications/initialized',{},true);
    tools=(await rpc('tools/list',{})).tools;
  }
  const call=async(name,args)=>{
    if(!['list_pages','list_console_messages','list_network_requests','performance_start_trace','performance_stop_trace'].includes(name)||!tools.some(t=>t.name===name))throw Error('MCP tool unavailable or denied');
    const result=await rpc('tools/call',{name,arguments:args});if(result.isError)throw Error(JSON.stringify(result));return result;
  };
  const pages=await call('list_pages',{});
  // Pin a specific CDP target by URL from our own Playwright page. The fresh
  // capsule has no personal tabs. Schema versions with/without pageId coexist.
  const content=(pages.content||[]).filter(c=>c.type==='text').map(c=>c.text).join('\n');
  const line=content.split('\n').find(s=>s.includes(page.url()));
  const match=line?.match(/(?:^|\s)(\d+)[:\s]/);if(!match)throw Error('Owned MCP page identity not found');
  const pageId=Number(match[1]);
  const argsFor=(name,args)=>tools.find(t=>t.name===name).inputSchema.properties.pageId?{...args,pageId}:args;
  if(mode==='diagnostics')return {identity:mcpIdentity,pages,console:await call('list_console_messages',argsFor('list_console_messages',{})),network:await call('list_network_requests',argsFor('list_network_requests',{})),outcome:'PASS'};
  const tracePath=path.join(directory,`devtools-${++sequence}.json.gz`);
  const start=await call('performance_start_trace',argsFor('performance_start_trace',{reload:false,autoStop:false}));
  await page.reload({waitUntil:'load'});
  const stop=await call('performance_stop_trace',argsFor('performance_stop_trace',{filePath:tracePath}));
  if(!fs.existsSync(tracePath))throw Error('MCP did not preserve requested trace');
  return {outcome:'PASS',identity:mcpIdentity,start,stop,trace:adapter.binary('performance_trace',fs.readFileSync(tracePath)),measurement:'local_lab_trace_not_field_data'};
}
async function accessibility(page){
  await page.addScriptTag({path:'/opt/melissa-frontend/node_modules/axe-core/axe.min.js'});
  const result=await page.evaluate(async()=>{const r=await window.axe.run(document,{resultTypes:['violations','incomplete','passes','inapplicable']});return {testEngine:r.testEngine,url:r.url,timestamp:r.timestamp,violations:r.violations,incomplete:r.incomplete,passes:r.passes.map(p=>p.id),inapplicable:r.inapplicable.map(p=>p.id)};});
  return {...result,outcome:result.violations.length?'FAIL':'PASS',manual_review_required:result.incomplete.length>0,scope:'automated_rules_not_full_WCAG_conformance'};
}
async function verify(name){
  const check=config.checks[name];if(!check)throw Error('Unknown reviewed frontend check');
  const reports=[];let functional='PASS',a11y=check.accessibility?'PASS':'NOT_RUN',visual=check.visual.length?'PASS':'NOT_RUN';
  for(const viewport of check.viewports){
    const ctx=adapter.getContext(),page=await ctx.newPage();
    // One isolated context per bundle; each viewport has a new page. Backend
    // effects persist deliberately; scenarios must define their own fixture reset.
    await page.setViewportSize({width:viewport.width,height:viewport.height});page.setDefaultTimeout(10000);
    const report={viewport,steps:[],checkpoints:[],functionality:'PASS'};
    page.on('console',m=>adapter.record({kind:'console',type:m.type(),text:m.text(),viewport:viewport.name}));
    page.on('pageerror',e=>adapter.record({kind:'pageerror',message:String(e),viewport:viewport.name}));
    page.on('response',r=>adapter.record({kind:'response',url:r.url(),status:r.status(),viewport:viewport.name}));
    try{
      for(const [index,step] of check.steps.entries()){
        switch(step.action){
          case 'navigate':{const url=new URL(step.path,config.origin);if(url.origin!==config.origin)throw Error('foreign navigation');const r=await page.goto(url.href,{waitUntil:'domcontentloaded'});if(!r||r.status()>=400)throw Error('navigation did not succeed');break;}
          case 'fill':await page.locator(step.selector).fill(step.value);break;
          case 'click':await page.locator(step.selector).click();break;
          case 'press':await page.locator(step.selector).press(step.key);break;
          case 'wait':await page.locator(step.selector).waitFor({state:step.state,timeout:step.timeout_ms});break;
          case 'assert_text':{const loc=page.locator(step.selector);await loc.waitFor({state:'visible'});await page.waitForFunction(({selector,text})=>document.querySelector(selector)?.textContent?.trim()===text,{selector:step.selector,text:step.text});break;}
          case 'assert_visible':if(await page.locator(step.selector).isVisible()!==step.visible)throw Error('visibility assertion failed');break;
          case 'checkpoint':{
            await page.evaluate(()=>document.fonts.ready);const capture={name:step.name,viewport:viewport.name,url:page.url(),dimensions:{width:viewport.width,height:viewport.height},browser_version:adapter.getBrowser().version(),playwright:require('/opt/melissa-web-tools/node_modules/playwright/package.json').version,service:config.service,environment_digest:config.environment_digest};
            const png=await page.screenshot({fullPage:true,animations:'disabled',caret:'hide'});capture.screenshot={...adapter.binary('png',png),capture:{...capture}};
            if(check.accessibility){try{capture.accessibility=await accessibility(page);if(capture.accessibility.outcome!=='PASS')a11y='FAIL';}catch(error){capture.accessibility={outcome:'BLOCKED',reason:String(error)};if(a11y!=='FAIL')a11y='BLOCKED';}}
            const accepted=check.visual.find(v=>v.viewport===viewport.name&&v.checkpoint===step.name);
            if(accepted){
              const baseline=config.baselines[accepted.baseline_id];if(!baseline)throw Error('accepted baseline unavailable');
              const PNG=require('/opt/melissa-frontend/node_modules/pngjs').PNG;
              const current=PNG.sync.read(png),previous=PNG.sync.read(Buffer.from(baseline.png_hex,'hex'));
              const sameEnvironment=baseline.capture.browser_version===capture.browser_version&&baseline.capture.playwright===capture.playwright&&baseline.capture.service===capture.service&&baseline.capture.dimensions.width===capture.dimensions.width&&baseline.capture.dimensions.height===capture.dimensions.height&&baseline.capture.name===capture.name&&baseline.capture.url===capture.url&&baseline.capture.environment_digest===capture.environment_digest;
              if(!sameEnvironment){capture.visual={outcome:'BLOCKED',reason:'baseline_capture_environment_mismatch',baseline_id:accepted.baseline_id};if(visual!=='FAIL')visual='BLOCKED';}
              else if(current.width!==previous.width||current.height!==previous.height){capture.visual={outcome:'FAIL',reason:'image_dimensions_changed',baseline_id:accepted.baseline_id};visual='FAIL';}
              else{const diff=new PNG({width:current.width,height:current.height});const pixelmatch=(await import('/opt/melissa-frontend/node_modules/pixelmatch/index.js')).default;const pixels=pixelmatch(previous.data,current.data,diff.data,current.width,current.height,{threshold:0.1});const ratio=pixels/(current.width*current.height);capture.visual={outcome:ratio<=accepted.max_diff_ratio?'PASS':'FAIL',baseline_id:accepted.baseline_id,different_pixels:pixels,diff_ratio:ratio,max_diff_ratio:accepted.max_diff_ratio,diff:adapter.binary('visual_diff',PNG.sync.write(diff))};if(capture.visual.outcome!=='PASS')visual='FAIL';}
            }
            report.checkpoints.push(capture);break;
          }
          default:throw Error('unreviewed frontend action');
        }
        report.steps.push({index,action:step.action,outcome:'PASS'});
      }
    }catch(error){functional='FAIL';report.functionality='FAIL';report.steps.push({index:report.steps.length,outcome:'FAIL',reason:String(error)});report.failure_screenshot=adapter.binary('png',await page.screenshot({fullPage:true}).catch(()=>Buffer.alloc(0)));}
    report.events=adapter.takeEvents();
    if(check.fail_on_console_error&&report.events.events.some(e=>e.kind==='pageerror'||e.kind==='console'&&e.type==='error')){functional='FAIL';report.functionality='FAIL';}
    if(check.accessibility&&report.checkpoints.length===0&&a11y==='PASS')a11y='NOT_RUN';
    if(check.visual.some(v=>v.viewport===viewport.name&&!report.checkpoints.some(c=>c.name===v.checkpoint&&c.visual))&&visual==='PASS')visual='NOT_RUN';
    reports.push(report);await page.close();
  }
  return {name,outcome:[functional,a11y,visual].includes('FAIL')?'FAIL':[functional,a11y,visual].includes('BLOCKED')?'BLOCKED':'PASS',functionality:functional,accessibility:a11y,visual,viewports:reports,baseline_updated:false};
}
async function main(op){
  if(op.command==='start')return adapter.main(op);
  if(op.command==='capabilities')return {origin:config.origin,allowed_origins:config.allowedOrigins,websocket:config.websocket,service_workers:config.serviceWorkers,devtools:config.devtools,external_network:false,service_worker_interception:'partial; outer capsule network isolation remains authoritative'};
  if(op.command==='verify')return verify(op.name);
  if(op.command==='accessibility')return accessibility(await adapter.pageFor(op.page));
  if(op.command==='devtools')return devtools(await adapter.pageFor(op.page),op.mode);
  const value=await adapter.main(op);
  if(op.command==='close'){
    value.event_log=adapter.binary('browser_events',fs.readFileSync(eventFile));
    if(mcp){mcp.stdin.end();mcp.kill('SIGTERM');value.devtools_log=adapter.binary('devtools_log',fs.readFileSync(path.join(directory,'devtools.log')));}
  }
  return value;
}
(async()=>{for await(const line of readline.createInterface({input:process.stdin,crlfDelay:Infinity})){let op;try{op=JSON.parse(line);const body=await main(op);process.stdout.write(JSON.stringify({ok:true,body})+'\n');}catch(e){process.stdout.write(JSON.stringify({ok:false,error:{kind:'browser_error',message:String(e)}})+'\n');}if(op?.command==='close')break;}})().catch(()=>{process.exitCode=1;});
