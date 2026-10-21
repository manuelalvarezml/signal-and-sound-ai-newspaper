import json
from datetime import datetime, timedelta
from pathlib import Path
from .collect import collect
from .compose import Composer
from .models import Edition
from .interests import ZONE, coverage
from .render import render, public_edition
from .state import Budget, claim, finish

ROOT=Path(__file__).resolve().parent.parent

def config(name):
    path=ROOT/'config'/f'{name}.json'
    if not path.exists(): path=ROOT/'config'/f'{name}.example.json'
    return json.loads(path.read_text())

def analyze(settings, profile, state, store, at, kind='daily'):
    run_id=f'{kind}-{at.astimezone(ZONE).date()}'
    ready=store.json(f'editions/{run_id}.json')
    if ready: return Edition.model_validate(ready)
    if not claim(state,'analysis:'+run_id): raise RuntimeError('Run already active or completed')
    collection=collect(config('sources'),settings,profile,at,Budget(state,settings),store,run_id,weekly=kind=='weekly')
    history=[]
    historical_sources={}
    lower=(at-timedelta(days=35)).date().isoformat()
    for key in sorted(store.keys('editions/'))[-40:]:
        old=store.json(key)
        if old and lower<=old['date']<str(at.date()):
            historical_sources.update({s['id']:s for s in old['sources']})
            history.append({'date':old['date'],'kind':old['kind'],'pages':[{ 'id':p['id'],'stories':[{'title':s['title'],'status':s['status'],'summary':s['paragraphs'],'source_ids':s['source_ids']} for s in p['stories']]} for p in old['pages']],'sources':[{'id':s['id'],'url':s['url'],'published':s['published']} for s in old['sources']]})
    if kind=='weekly':
        _,end,_=coverage(at,kind,settings['started_at'])
        records={**historical_sources,**{s['id']:s for s in collection['records']}}
        collection={**collection,'records':[s for s in records.values() if s.get('published') and s['published'][:10]<end.date().isoformat()]}
    edition=Composer(settings,Budget(state,settings),store,run_id).compose(collection,profile,at,kind,history)
    store.json(f'editions/{run_id}.json',edition.model_dump(mode='json'))
    finish(state,'analysis:'+run_id)
    return edition

def publish(edition,dist,settings,state):
    # Synchronous render validates the PDF before changing latest.json.
    path=render(edition,dist,settings['site_url'])
    finish(state,'publish:'+edition.id,revision=edition.revision)
    return path
