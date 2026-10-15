"""Validate actual authoritative tool events, independently of model intent."""
def command_facts(records, marker, expected):
    calls=[]
    results={}
    for event in records:
        body=event.payload
        payload=body.get('payload',{})
        if body.get('type')=='tool_call' and payload.get('toolName')=='bash':
            command=payload.get('normalizedInput',{}).get('command','')
            if marker in command:
                calls.append(payload)
        if body.get('type')=='tool_result':
            results.setdefault(payload.get('callId'),[]).append(payload)
    ids=[call.get('callId') for call in calls]
    if len(calls)!=expected or len(set(ids))!=expected or None in ids:
        raise RuntimeError('workload lacks exact distinct authoritative command calls')
    for index,call in enumerate(calls):
        token=marker+('%04d'%index if marker=='acceptance-long-' else '')
        expected_command='printf '+token
        if marker=='acceptance-long-':
            expected_command="printf '%%s\\n' %04d >> /mnt/data/retained-long-run-markers.txt; printf acceptance-long-%04d" % (index,index)
        matched=results.get(call['callId'],[])
        if (call.get('normalizedInput',{}).get('command')!=expected_command
                or len(matched)!=1 or matched[0].get('resultState')!='successWithOutput'
                or token not in matched[0].get('modelContent','')):
            raise RuntimeError('workload lacks a unique successful actual result for each command')
    return {'attemptedCommands':len(calls),'completedCommands':len(calls),'distinctCallIds':len(set(ids))}


def instructions_facts(lines, expected_digest):
    import json
    observations=[]
    for line in lines:
        try:row=json.loads(line)
        except (ValueError,TypeError):continue
        if isinstance(row,dict) and row.get('kind')=='automaticInstructionsObserved':
            index=row.get('commandIndex')
            if (type(index) is not int or not 0<=index<1000
                    or row.get('instructionSha256')!=expected_digest):
                raise RuntimeError('model request instructions observation has wrong identity')
            observations.append(row)
    if {row['commandIndex'] for row in observations}!=set(range(1000)):
        raise RuntimeError('automatic AGENTS.md instructions were not observed for all long-work requests')
    return {'observations':len(observations),'distinctCommandIndices':1000,'instructionSha256':expected_digest}


def agents_manifest_facts(records, manifest_rows, content_rows, instructions):
    import hashlib,json
    def decode(digest,raw,domain):
        if digest!='sha256:'+hashlib.sha256(('centaeris.model_observation_'+domain+'.v1\0').encode()+raw.encode()).hexdigest():
            raise RuntimeError('authoritative observation digest mismatch')
        return json.loads(raw)
    manifests={key:decode(key,raw,'manifest') for key,raw in manifest_rows}
    contents={key:decode(key,raw,'content') for key,raw in content_rows}
    roots=[]
    for event in records:
        payload=event.payload.get('payload',{})
        if event.payload.get('type')=='model_request_started' and payload.get('purpose')=='main':
            ref=payload.get('observations',{})
            if not isinstance(ref,dict) or set(ref)!={'manifestDigest'}:
                raise RuntimeError('main request lacks authoritative observation manifest')
            roots.append(ref['manifestDigest'])
    if len(roots)<1000:raise RuntimeError('long Run lacks enough actual main requests')
    wanted='# AGENTS.md instructions for /mnt/data\n\n<INSTRUCTIONS>\n'+instructions+'\n</INSTRUCTIONS>'
    last_root=None;references=[];validated=0
    for root in roots:
        cursor=root;chain=[];seen=set()
        while cursor!=last_root:
            if cursor is None:
                references=[];break
            if cursor in seen or cursor not in manifests:
                raise RuntimeError('observation manifest parent missing or cyclic')
            seen.add(cursor);manifest=manifests[cursor]
            if set(manifest)!={'parentDigest','observationCount','changes'}:
                raise RuntimeError('observation manifest fields invalid')
            chain.append(manifest);cursor=manifest['parentDigest']
        for manifest in reversed(chain):
            count=manifest['observationCount'];changed=set()
            if type(count) is not int or count<0:raise RuntimeError('observation count invalid')
            for change in manifest['changes']:
                index=change.get('index')
                if (type(index) is not int or not 0<=index<count or index>len(references)
                        or index in changed or set(change)!={'index','kind','contentDigest'}):
                    raise RuntimeError('observation change invalid or incomplete')
                changed.add(index)
                if index==len(references):references.append(change)
                else:references[index]=change
            references=references[:count]
            if len(references)!=count:raise RuntimeError('observation references incomplete')
        matched=False
        for ref in references:
            value=contents.get(ref['contentDigest']) if ref['kind']=='message' else None
            message=(value or {}).get('message',{})
            if (message.get('messageId','').endswith(':agents_context')
                    and message.get('role')=='user' and message.get('content')==wanted):
                matched=True;break
        if not matched:raise RuntimeError('main request lacks exact authoritative AGENTS.md content')
        validated+=1;last_root=root
    return {'mainRequestsWithAgentsInstructions':validated,
        'agentsFileSha256':hashlib.sha256(instructions.encode()).hexdigest(),
        'observationManifestDigests':len(set(roots))}
