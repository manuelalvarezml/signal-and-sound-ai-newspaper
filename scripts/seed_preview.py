"""Initialize a local example without overwriting personal settings."""
import shutil
from pathlib import Path
ROOT=Path(__file__).resolve().parent.parent
(ROOT/'private').mkdir(exist_ok=True)
for source,target in [('tests/fixtures/edition.json','private/today.json'),('config/interests.example.json','config/interests.json')]:
    destination=ROOT/target
    if not destination.exists(): shutil.copyfile(ROOT/source,destination)
print('Example ready; run python -m newspaper run --edition private/today.json')
