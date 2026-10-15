#!/usr/bin/env python3
"""Generate an aggregate JSON schema and Rust protocol metadata from pinned Codex inputs.

Source: openai/codex@687a119f0fcaace47e1f1abcc77cec6c813fd6da,
matching upstream/packages/effect-codex-app-server/scripts/generate.ts.
"""
import json, pathlib, re, subprocess, os
ROOT = pathlib.Path(__file__).resolve().parents[1]
PIN = ROOT / 'schema-pinned'
OUT = ROOT / 'src/generated'
files = sorted(p for p in (PIN/'json').rglob('*.json') if not p.name.startswith('codex_app_server_protocol.'))

def name(p):
    relative = p.relative_to(PIN/'json')
    return (relative.parts[0].capitalize() if len(relative.parts)>1 else '') + p.stem

file_names = {p: name(p) for p in files}
qualified = {str(p.relative_to(PIN/'json').with_suffix('')): name(p) for p in files}
result = {}

def rewrite(node, local, namespace):
    if isinstance(node, list): return [rewrite(x,local,namespace) for x in node]
    if not isinstance(node,dict): return node
    out={}
    for k,v in node.items():
        if k=='$ref' and isinstance(v,str) and v.startswith('#/definitions/'):
            key=v[len('#/definitions/'):]
            if key in local: target=local[key]
            else:
                candidates=([namespace+'/'+key] if namespace else [])+[key,re.sub(r'^(v1|v2|serde_json)/','',key),'v2/'+key,'v1/'+key,'serde_json/'+key]
                target=next((qualified[c] for c in candidates if c in qualified),None)
                if target is None: raise ValueError(f'unresolved {v}')
            out[k]='#/definitions/'+target
        elif k=='default' and v is None: continue
        else: out[k]=rewrite(v,local,namespace)
    if isinstance(out.get('type'),list) and len(out['type'])==2 and 'null' in out['type']:
        kind=next(x for x in out['type'] if x!='null')
        out={**{k:v for k,v in out.items() if k!='type'}, 'anyOf':[{**{k:v for k,v in out.items() if k!='type'},'type':kind},{'type':'null'}]}
    return out

for p in files:
    doc=json.loads(p.read_text())
    prefix=file_names[p]
    local={n:prefix+'__'+re.sub(r'[^A-Za-z0-9]','',n) for n in doc.get('definitions',{})}
    namespace=p.relative_to(PIN/'json').parts[0] if len(p.relative_to(PIN/'json').parts)>1 else None
    for n,schema in doc.get('definitions',{}).items():
        if n=='PlanType': schema={'type':'string'}
        result[local[n]]=rewrite(schema,local,namespace)
    result[prefix]=rewrite({k:v for k,v in doc.items() if k!='definitions'},local,namespace)

# Match Effect's handful of manual schemas absent from the Codex schema source.
manual={
 'GetAuthStatusParams': {'type':'object','properties':{'includeToken':{'type':['boolean','null']},'refreshToken':{'type':['boolean','null']}}},
 'GetConversationSummaryParams': {'oneOf':[{'type':'object','properties':{'rolloutPath':{'type':'string'}},'required':['rolloutPath']},{'type':'object','properties':{'conversationId':{'type':'string'}},'required':['conversationId']}]},
 'GetConversationSummaryResponse': {'type':'object','properties':{'summary':{}},'required':['summary']},
 'GitDiffToRemoteParams': {'type':'object','properties':{'cwd':{'type':'string'}},'required':['cwd']},
 'GitDiffToRemoteResponse': {'type':'object','properties':{'sha':{'type':'string'},'diff':{'type':'string'}},'required':['sha','diff']},
 'GetAuthStatusResponse': {'type':'object','properties':{'authMethod':{},'authToken':{'type':['string','null']},'requiresOpenaiAuth':{'type':['boolean','null']}},'required':['authMethod','authToken','requiresOpenaiAuth']}
}
for k,v in manual.items(): result.setdefault(k,rewrite(v,{},None))
OUT.mkdir(exist_ok=True)
(OUT/'codex.schema.json').write_text(json.dumps({'$schema':'http://json-schema.org/draft-07/schema#','title':'CodexSchema','type':'object','definitions':result},sort_keys=True,separators=(',',':'))+'\n')

request_re=re.compile(r'\{\s*"method":\s*"([^"]+)",\s*id:\s*RequestId,\s*params(\??):\s*([^,}|]+)')
notification_re=re.compile(r'\{\s*"method":\s*"([^"]+)"(?:,\s*"params":\s*([^ }]+))?\s*\}')

def entries(file,kind):
    text=(PIN/'typescript'/file).read_text()
    if kind=='request': return [(m,'Nullable'+t.strip() if opt else t.strip()) for m,opt,t in request_re.findall(text)]
    return [(m,t.strip() if t else 'undefined') for m,t in notification_re.findall(text)]

def resolve(raw):
    if raw=='undefined': return None
    for candidate in (raw,'V2'+raw,'V1'+raw,'SerdeJson'+raw):
        if candidate in result: return candidate
    raise ValueError('unresolved type '+raw)

overrides={'account/gatewayOAuth/cancel':'GatewayOAuthCancelResponse','account/gatewayOAuth/login':'GatewayOAuthLoginResponse','account/gatewayOAuth/read':'GatewayOAuthReadResponse','account/logout':'LogoutAccountResponse','account/rateLimits/read':'GetAccountRateLimitsResponse','account/usage/read':'GetAccountTokenUsageResponse','account/workspaceMessages/read':'GetWorkspaceMessagesResponse','config/batchWrite':'ConfigWriteResponse','config/mcpServer/reload':'McpServerRefreshResponse','config/value/write':'ConfigWriteResponse','configRequirements/read':'ConfigRequirementsReadResponse','externalAgentConfig/import/readHistories':'ExternalAgentConfigImportHistoriesReadResponse'}
def response(method,params):
    if method in overrides: return resolve(overrides[method])
    if params and params!='undefined':
        try: return resolve(re.sub(r'Params$','Response',params))
        except ValueError: pass
    pascal=''.join(s[:1].upper()+s[1:] for s in re.split(r'[/_-]|(?=[A-Z])',method) if s)
    return resolve(pascal+'Response')

cats=[('CLIENT_REQUEST',entries('ClientRequest.ts','request')),('CLIENT_NOTIFICATION',entries('ClientNotification.ts','notification')),('SERVER_REQUEST',entries('ServerRequest.ts','request')),('SERVER_NOTIFICATION',entries('ServerNotification.ts','notification'))]
meta=['//! Generated from pinned Codex schema/type declarations. Do not edit.','#[derive(Debug, Clone, Copy, PartialEq, Eq)]','pub struct MethodMeta { pub method: &\'static str, pub params: Option<&\'static str>, pub response: Option<&\'static str> }']
for cat,items in cats:
    meta.append(f'pub const {cat}_METHODS: &[MethodMeta] = &[')
    for method,params in items:
        param=resolve(params)
        resp=response(method,params) if cat.endswith('REQUEST') else None
        fmt=lambda x: 'Some("'+x+'")' if x else 'None'
        meta.append(f'    MethodMeta {{ method: {json.dumps(method)}, params: {fmt(param)}, response: {fmt(resp)} }},')
    meta.append('];')
meta.append("pub fn method_meta<'a>(methods: &'a [MethodMeta], method: &str) -> Option<&'a MethodMeta> { methods.iter().find(|entry| entry.method == method) }")
(OUT/'meta.rs').write_text('\n'.join(meta)+'\n')
namespaces=['//! Generated namespace names from pinned Codex JSON schema files.']
for ns in ['v1','v2']:
    arr=sorted((p.stem,file_names[p]) for p in files if p.relative_to(PIN/'json').parts[0]==ns)
    namespaces.append(f'pub const {ns.upper()}: &[(&str, &str)] = &[')
    namespaces += [f'    ({json.dumps(a)}, {json.dumps(b)}),' for a,b in arr]
    namespaces.append('];')
(OUT/'namespaces.rs').write_text('\n'.join(namespaces)+'\n')
types=['// Generated serde wrappers from pinned Codex schemas. Do not edit.', 'use serde::{Deserialize, Serialize};', 'use serde_json::Value;']
for type_name in sorted(result):
    types.extend(['#[allow(non_camel_case_types)]', '#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]', '#[serde(transparent)]', f'pub struct {type_name}(pub Value);', f'impl TryFrom<Value> for {type_name} {{', '    type Error = String;', f'    fn try_from(value: Value) -> Result<Self, Self::Error> {{ crate::schema::validate_type("{type_name}", &value)?; Ok(Self(value)) }}', '}', ''])
(OUT/'types.generated.rs').write_text('\n'.join(types))
subprocess.run([os.environ.get('RUSTFMT', str(pathlib.Path.home()/'.cargo/bin/rustfmt')), '--edition', '2024', str(OUT/'meta.rs'), str(OUT/'namespaces.rs'), str(OUT/'types.generated.rs')], check=True)
print(f'{len(result)} schemas, '+', '.join(f'{cat}={len(items)}' for cat,items in cats))
