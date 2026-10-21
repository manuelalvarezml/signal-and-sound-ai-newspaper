import json
from concurrent.futures import ThreadPoolExecutor
from datetime import datetime
from pathlib import Path
import pytest
from pypdf import PdfReader
from newspaper.state import State,Budget,BudgetExceeded,claim,finish,Store
from newspaper.interests import coverage,apply_observations,ZONE,public_interest_config
from newspaper.collect import canonical,x_score,assert_public,parse_source
from newspaper.models import Edition
from newspaper.render import render,public_edition
from newspaper.pipeline import config,ROOT

@pytest.fixture
def edition(): return Edition.model_validate_json((ROOT/'tests/fixtures/edition.json').read_text())

def test_first_week_and_normal_week():
    s,e,partial=coverage(datetime(2026,10,26,6,tzinfo=ZONE),'weekly',config('settings')['started_at'])
    assert s.isoformat()=='2026-10-21T00:00:00-05:00' and e.day==26 and partial
    s,e,partial=coverage(datetime(2026,11,2,6,tzinfo=ZONE),'weekly',config('settings')['started_at'])
    assert (s.day,e.day,partial)==(26,2,False)
    with pytest.raises(ValueError):coverage(datetime(2026,10,21,6,tzinfo=ZONE),'weekly',config('settings')['started_at'])

def test_budget_atomic_and_replay(tmp_path):
    settings={**config('settings'),'stop_usd':1.5,'infrastructure_reserve_usd':1,'x_monthly_usd':1}
    budget=Budget(State(tmp_path),settings)
    def reserve(i):
        try:budget.reserve(str(i),'x',.1);return True
        except BudgetExceeded:return False
    with ThreadPoolExecutor(max_workers=8) as p: results=list(p.map(reserve,range(20)))
    assert sum(results)==5
    with pytest.raises(BudgetExceeded): budget.reserve('0','x',.1)

def test_run_leases(tmp_path):
    state=State(tmp_path);assert claim(state,'daily');assert not claim(state,'daily')
    finish(state,'daily');assert not claim(state,'daily')

def test_interest_provenance_and_weekly_bound():
    profile={'version':'a','topics':[{'id':'voice','weight':.5,'explicit':False},{'id':'music','weight':.9,'explicit':True}]}
    now=datetime(2026,10,21,tzinfo=ZONE)
    observations=[{'topic':topic,'source':str(i),'date':'2026-10-21','kind':'work','reason':'human request'} for i,topic in enumerate(['voice','voice','music'])]
    new,log=apply_observations(profile,observations,now)
    assert new['topics'][0]['weight']==.55 and new['topics'][1]['weight']==.9
    again,log=apply_observations(new,observations,now);assert log==[]
    automated={**observations[0],'source':'heartbeat','automated':True}
    _,log=apply_observations(new,[automated],now);assert log==[]

def test_x_engagement_cannot_replace_relevance():
    popular=x_score(0,0,1,.3,1,{'like_count':1000000})
    pertinent=x_score(1,1,1,.8,5,{'like_count':0})
    assert popular<65<pertinent
    assert x_score(1,1,1,.8,100,{})<pertinent

def test_url_safety():
    assert canonical('https://EXAMPLE.com/a/?utm_source=x&id=2#top')=='https://example.com/a?id=2'
    for url in ['file:///etc/passwd','http://127.0.0.1/','http://169.254.169.254/','https://user:pass@example.com/']:
        with pytest.raises(ValueError):assert_public(url)

def test_feed_parsing():
    class Fetch:
        def get(self,*a,**kw):return '<rss><channel><item><title>AI paper</title><link>https://example.org/paper</link><pubDate>Wed, 21 Oct 2026 10:00:00 GMT</pubDate><description>Evidence.</description></item></channel></rss>'
    records=parse_source({'url':'https://example.org/rss','kind':'rss','name':'Example','page':'ai','authority':.8},Fetch(),datetime(2026,10,21,12,tzinfo=ZONE))
    assert len(records)==1 and records[0].excerpt=='Evidence.'

def test_bad_citations_and_stale_labels_fail(edition):
    data=edition.model_dump(mode='json');data['pages'][0]['stories'][0]['source_ids']=['invented']
    with pytest.raises(ValueError):Edition.model_validate(data)
    data=edition.model_dump(mode='json');data['pages'][0]['stories'][2]['status']='New'
    with pytest.raises(ValueError):Edition.model_validate(data)

def test_public_redaction(edition):
    data=public_edition(edition)
    assert all('excerpt' not in s for s in data['sources'])
    assert all('evidence' not in s for p in data['pages'] for s in p['stories'])

def test_render_two_pages_and_archive(tmp_path,edition):
    target=render(edition,tmp_path)
    reader=PdfReader(target/'signal-and-sound.pdf');assert len(reader.pages)==2
    assert all(float(p.mediabox.width)==pytest.approx(595.2756,abs=.1) for p in reader.pages)
    for i,page in enumerate(edition.pages):
        text=reader.pages[i].extract_text()
        assert all(s.title in text.replace('\n',' ') for s in page.stories)
    pointer=json.loads((tmp_path/'latest.json').read_text());assert pointer['daily']['id']==edition.id
    assert len(json.loads((tmp_path/'archive.json').read_text()))==1
    assert (target/'ai.html').read_text().count('data-concept=')>=2

def test_failed_render_retains_pointer(tmp_path,edition):
    render(edition,tmp_path);old=(tmp_path/'latest.json').read_text()
    overflowing=edition.model_copy(deep=True);overflowing.revision=2
    overflowing.pages[0].stories[0].paragraphs=['overflow '*1000]
    with pytest.raises(ValueError):render(overflowing,tmp_path)
    assert (tmp_path/'latest.json').read_text()==old

def test_storage_path_boundary(tmp_path):
    store=Store(tmp_path)
    with pytest.raises(ValueError):store.put('../outside.json','bad')

def test_composer_paid_checkpoints_and_grounding(tmp_path,edition,monkeypatch):
    from newspaper.compose import Composer
    import boto3
    responses=[{'items':[{'source_id':s.id,'facts':[s.excerpt],'limitations':[],'topic':s.page} for s in edition.sources]},edition.model_dump(mode='json'),{'supported':True,'issues':[]}]
    class Fake:
        calls=0
        def converse(self,**kwargs):
            value=responses[self.calls];self.calls+=1
            return {'stopReason':'end_turn','output':{'message':{'content':[{'text':json.dumps(value)}]}},'usage':{'inputTokens':100,'outputTokens':100}}
    fake=Fake();monkeypatch.setattr(boto3,'client',lambda *a,**kw:fake)
    state=State(tmp_path/'state');store=Store(tmp_path/'evidence');budget=Budget(state,config('settings'))
    composer=Composer(config('settings'),budget,store,edition.id)
    collection={'records':[s.model_dump() for s in edition.sources],'coverage_notes':[]}
    first=composer.compose(collection,config('interests'),edition.cutoff,'daily',[])
    second=composer.compose(collection,config('interests'),edition.cutoff,'daily',[])
    assert first==second and fake.calls==3
    assert len(state.read(budget.key)['requests'])==3

def test_revision_cannot_corrupt_live_artifacts(tmp_path,edition):
    target=render(edition,tmp_path);original=(target/'signal-and-sound.pdf').read_bytes()
    changed=edition.model_copy(deep=True);changed.pages[0].stories[0].title='Changed'
    with pytest.raises(ValueError,match='immutable'):render(changed,tmp_path)
    assert (target/'signal-and-sound.pdf').read_bytes()==original

def test_template_is_disabled_and_has_no_private_upload():
    import yaml
    class Loader(yaml.SafeLoader):pass
    def tags(loader,tag,node):
        if isinstance(node,yaml.ScalarNode):return {tag:loader.construct_scalar(node)}
        return {tag:loader.construct_sequence(node)}
    Loader.add_multi_constructor('!',tags)
    data=yaml.load((ROOT/'infra/template.yaml').read_text(),Loader=Loader)
    assert data['Parameters']['ProductionEnabled']['Default']=='false'
    assert data['Parameters']['DeliveryEnabled']['Default']=='false'
    schedule=data['Resources']['Collector']['Properties']['Events']['Morning']['Properties']
    assert schedule['ScheduleExpression']=='cron(0 6 * * ? *)' and schedule['ScheduleExpressionTimezone']=='America/Lima'
    assert 'private' not in (ROOT/'Dockerfile').read_text()

def test_refined_print_and_web_layout(tmp_path,edition):
    target=render(edition,tmp_path)
    html=(target/'ai.html').read_text()
    assert edition.pages[0].subtitle not in html
    assert 'INDEPENDENT CURIOSITY' not in html and 'ONE QUIETER MORNING' not in html
    assert 'print.pdf' in html and 'download="signal-and-sound-' in html
    assert len(edition.pages[0].stories)==len(edition.pages[1].stories)==6
    assert (target/'print.pdf').read_bytes()==(target/'signal-and-sound.pdf').read_bytes()
    for pdf_page,page,label in zip(PdfReader(target/'signal-and-sound.pdf').pages,edition.pages,['General AI','AI for Music']):
        text=pdf_page.extract_text()
        assert label in text and 'MIN READ' in text and "EDITOR'S NOTE" in text
        assert page.title not in text and page.subtitle not in text
        ids=list(dict.fromkeys(c for s in page.stories for c in s.concepts))[:2]
        for cid in ids:
            concept=next(c for c in edition.concepts if c.id==cid)
            assert concept.why_it_matters in text.replace('\n',' ')


def test_publisher_serializes_and_releases_after_failure(tmp_path,monkeypatch):
    from newspaper import handlers
    from newspaper.state import claim
    state=State(tmp_path/'state')
    monkeypatch.setattr(handlers,'runtime',lambda: ({},state,None))
    assert claim(state,'publication-lock',150)
    with pytest.raises(RuntimeError,match='Publication busy'):
        handlers.publisher({},None)
    state.mutate('run:publication-lock',lambda v: ({'status':'idle','lease_until':0},None))
    def fail(*args):raise ValueError('render failure')
    monkeypatch.setattr(handlers,'_publish',fail)
    with pytest.raises(ValueError,match='render failure'):
        handlers.publisher({},None)
    assert claim(state,'publication-lock',150)


def test_editorial_revision_preserves_earlier_source_cutoff(tmp_path,edition):
    from datetime import timedelta
    render(edition,tmp_path)
    corrected=edition.model_copy(deep=True)
    corrected.revision+=1
    corrected.cutoff-=timedelta(minutes=1)
    render(corrected,tmp_path)
    pointer=json.loads((tmp_path/'latest.json').read_text())['daily']
    assert pointer['revision']==corrected.revision
    older=edition.model_copy(deep=True)
    older.cutoff+=timedelta(minutes=1)
    render(older,tmp_path/'other')
    # A later timestamp cannot downgrade an already-published revision.
    (tmp_path/'other'/'latest.json').write_text(json.dumps({'daily':pointer}))
    render(older,tmp_path/'other')
    assert json.loads((tmp_path/'other'/'latest.json').read_text())['daily']==pointer


def test_composer_corrects_invalid_draft_once(tmp_path,edition,monkeypatch):
    from newspaper.compose import Composer
    import boto3
    bad=edition.model_dump(mode='json');bad['pages'][0]['stories']=bad['pages'][0]['stories'][:2]
    responses=[{'items':[]},bad,edition.model_dump(mode='json'),{'supported':True,'issues':[]}]
    class Fake:
        calls=0
        def converse(self,**kwargs):
            value=responses[self.calls];self.calls+=1
            return {'stopReason':'end_turn','output':{'message':{'content':[{'text':json.dumps(value)}]}},'usage':{'inputTokens':100,'outputTokens':100}}
    fake=Fake();monkeypatch.setattr(boto3,'client',lambda *a,**kw:fake)
    state=State(tmp_path/'state');store=Store(tmp_path/'evidence')
    composer=Composer(config('settings'),Budget(state,config('settings')),store,edition.id)
    result=composer.compose({'records':[s.model_dump() for s in edition.sources],'coverage_notes':[]},config('interests'),edition.cutoff,'daily',[])
    assert len(result.pages[0].stories)==6 and fake.calls==4
    assert store.json(f'checkpoints/{edition.id}/write-gptoss-repair-response.json')


def test_composer_normalizes_page_local_concepts(edition):
    from newspaper.compose import Composer
    raw=edition.model_dump(mode='json');raw['pages'][0]['concepts']=raw['concepts'];raw['concepts']=[]
    result=Composer.validate(raw,{},[s.model_dump() for s in edition.sources])
    assert result.concepts==edition.concepts
    assert 'concepts' in raw['pages'][0]  # Normalization does not mutate cached evidence.


def test_composer_attaches_original_evidence_for_verification(edition):
    from newspaper.compose import Composer
    raw=edition.model_dump(mode='json');story=raw['pages'][0]['stories'][0];story['evidence']={}
    records=[s.model_dump() for s in edition.sources]
    result=Composer.validate(raw,{},records)
    sid=story['source_ids'][0]
    assert result.pages[0].stories[0].evidence[sid].endswith(next(s['excerpt'][:1200] for s in records if s['id']==sid))


def test_composer_labels_old_and_undated_sources(edition):
    from newspaper.compose import Composer
    raw=edition.model_dump(mode='json');story=raw['pages'][0]['stories'][0];story['status']='New'
    sid=story['source_ids'][0];records=[s.model_dump() for s in edition.sources]
    for source in records:
        if source['id']==sid:source['published']='2020-01-01'
    assert Composer.validate(raw,{},records).pages[0].stories[0].status=='Revisited'
    for source in records:
        if source['id']==sid:source['published']=None
    assert Composer.validate(raw,{},records).pages[0].stories[0].status=='Learning'


def test_pdf_handles_model_typographic_spaces_and_hyphens(tmp_path,edition):
    from newspaper.render import typography
    edited=edition.model_copy(deep=True)
    edited.pages[0].stories[0].title='Low\u2011latency tools for RealTime\u202f2'
    target=render(edited,tmp_path)
    assert typography(edited.pages[0].stories[0].title) in PdfReader(target/'signal-and-sound.pdf').pages[0].extract_text().replace('\n',' ')
