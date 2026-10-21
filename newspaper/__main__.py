import argparse,json
from pathlib import Path
from datetime import datetime
from functools import partial
from http.server import ThreadingHTTPServer,SimpleHTTPRequestHandler
from .pipeline import ROOT,config,analyze,publish
from .interests import ZONE,apply_observations,public_interest_config
from .models import Edition
from .state import State,Store,Budget

def main():
    p=argparse.ArgumentParser(description='Signal & Sound local newspaper')
    sub=p.add_subparsers(dest='command',required=True)
    run=sub.add_parser('run');run.add_argument('--kind',choices=['daily','weekly'],default='daily');run.add_argument('--cutoff');run.add_argument('--edition',type=Path,help='Source-checked editorial input, bypassing paid APIs')
    col=sub.add_parser('collect');col.add_argument('--cutoff')
    srv=sub.add_parser('serve');srv.add_argument('--port',type=int,default=8787)
    upd=sub.add_parser('interests');upd.add_argument('observations',type=Path)
    sub.add_parser('status')
    args=p.parse_args();settings=config('settings');state=State(ROOT/'state');store=Store(ROOT/'private/store')
    if args.command=='serve':
        class Handler(SimpleHTTPRequestHandler):
            def end_headers(self):
                if self.path.split('?')[0].endswith('.pdf'):
                    import re
                    match = re.search(r'\d{4}-\d{2}-\d{2}', self.path)
                    date = match.group(0) if match else 'edition'
                    mode = 'inline' if self.path.endswith('/print.pdf') else 'attachment'
                    self.send_header('Content-Disposition', f'{mode}; filename="signal-and-sound-{date}.pdf"')
                super().end_headers()

            def do_GET(self):
                if any(x.startswith('.') for x in self.path.split('/')): self.send_error(404);return
                super().do_GET()
        server=ThreadingHTTPServer(('127.0.0.1',args.port),partial(Handler,directory=str(ROOT/'dist')))
        print(f'Serving only public dist/ at http://localhost:{args.port}',flush=True);server.serve_forever()
    elif args.command=='interests':
        path=ROOT/'private/interests.json';profile=json.loads(path.read_text())
        updated,log=apply_observations(profile,json.loads(args.observations.read_text()),datetime.now(ZONE))
        (ROOT/'private/interest-history').mkdir(exist_ok=True)
        (ROOT/'private/interest-history'/f'{profile["version"]}.json').write_text(json.dumps(profile,indent=2))
        path.write_text(json.dumps(updated,indent=2));(ROOT/'config/interests.json').write_text(json.dumps(public_interest_config(updated),indent=2))
        with (ROOT/'private/interest-updates.jsonl').open('a') as f:
            for row in log:f.write(json.dumps(row)+'\n')
        print(json.dumps({'version':updated['version'],'changes':log},indent=2))
    elif args.command=='status': print(json.dumps({'settings':settings,'budget':state.read(Budget(state,settings).key)},indent=2))
    else:
        at=datetime.fromisoformat(args.cutoff) if args.cutoff else datetime.now(ZONE)
        if at.tzinfo is None: p.error('--cutoff must include a timezone')
        if args.command=='collect':
            from .collect import collect
            result=collect(config('sources'),settings,config('interests'),at,Budget(state,settings),store,'discovery-'+at.strftime('%Y-%m-%dT%H%M'))
            print(json.dumps({'records':len(result['records']),'candidates':result['metadata_candidates'],'coverage':result['coverage_notes']},indent=2));return
        if args.edition:
            edition=Edition.model_validate_json(args.edition.read_text())
            store.json(f'editions/{edition.id}.json',edition.model_dump(mode='json'))
        else: edition=analyze(settings,config('interests'),state,store,at,args.kind)
        print(publish(edition,ROOT/'dist',settings,state))
if __name__=='__main__':main()
