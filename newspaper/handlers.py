"""AWS entry points. No schedule or public delivery is enabled by default."""
import json,mimetypes,os,tempfile
from pathlib import Path
from datetime import datetime
from .pipeline import config,analyze
from .interests import ZONE
from .models import Edition
from .render import render
from .state import DynamoState,Store,claim


def runtime():
    settings=config('settings')
    if os.environ.get('PRODUCTION_ENABLED')!='true': raise RuntimeError('Production is disabled')
    settings['site_url']=os.environ['SITE_URL']
    state=DynamoState(os.environ['STATE_TABLE'])
    evidence=Store(bucket=os.environ['EVIDENCE_BUCKET'])
    return settings,state,evidence


def collector(event,context):
    import boto3
    settings,state,evidence=runtime()
    # Credentials, if connected later, live in encrypted SSM parameters, never files or logs.
    ssm=None
    for variable in ('BRAVE_API_KEY','X_BEARER_TOKEN'):
        parameter=os.environ.get(variable+'_PARAMETER')
        if parameter:
            ssm=ssm or boto3.client('ssm')
            os.environ[variable]=ssm.get_parameter(Name=parameter,WithDecryption=True)['Parameter']['Value']
    at=datetime.fromisoformat(event['cutoff']) if event.get('cutoff') else datetime.now(ZONE)
    at=at.astimezone(ZONE)
    profile=evidence.json('config/interests.json') or config('interests')
    kinds=[event['kind']] if event.get('kind') else ['daily']+(['weekly'] if at.weekday()==0 else [])
    results=[]
    for kind in kinds:
        edition=analyze(settings,profile,state,evidence,at,kind)
        # Rendering is causally triggered only after a validated analysis is durably saved.
        boto3.client('lambda').invoke(FunctionName=os.environ['PUBLISHER_FUNCTION'],InvocationType='Event',Payload=json.dumps({'edition_key':f'editions/{edition.id}.json'}).encode())
        results.append(edition.id)
    return {'analyzed':results}


def publisher(event,context):
    settings,state,evidence=runtime()
    if not claim(state,'publication-lock',seconds=150):
        raise RuntimeError('Publication busy; retry after current render')
    try:
        return _publish(event,settings,evidence)
    finally:
        state.mutate('run:publication-lock',lambda v: ({**v,'status':'idle','lease_until':0},None))


def _publish(event,settings,evidence):
    edition=Edition.model_validate(evidence.json(event['edition_key']))
    public=Store(bucket=os.environ['PUBLIC_BUCKET'])
    with tempfile.TemporaryDirectory() as directory:
        dist=Path(directory)
        # Recover only the small public index metadata needed to preserve past editions.
        old=public.json('latest.json')
        if old: (dist/'latest.json').write_text(json.dumps(old))
        for item in public.json('archive.json') or []:
            key=f'editions/{item["id"]}/r{item["revision"]}/edition.json'
            data=public.json(key)
            if data:
                p=dist/key;p.parent.mkdir(parents=True,exist_ok=True);p.write_text(json.dumps(data))
        render(edition,dist,settings['site_url'])
        # latest.json is the commit point. Failed uploads leave the preceding release usable.
        for file in sorted(dist.rglob('*')):
            if file.is_file() and file.name!='latest.json':
                key=str(file.relative_to(dist))
                disposition = None
                if key.endswith('.pdf'):
                    mode = 'inline' if key.endswith('/print.pdf') else 'attachment'
                    disposition = f'{mode}; filename="signal-and-sound-{edition.date}.pdf"'
                public.put(key,file.read_bytes(),mimetypes.guess_type(key)[0] or 'application/octet-stream',immutable=key.startswith(('editions/','assets/')),content_disposition=disposition)
        public.put('latest.json',(dist/'latest.json').read_bytes())
    return {'published':edition.id,'revision':edition.revision}
