"""Prepare a SAM template using one already-pushed image for both functions."""
import argparse,json
from pathlib import Path
import yaml
class Loader(yaml.SafeLoader):pass
def intrinsic(loader,tag,node):
    if isinstance(node,yaml.ScalarNode):value=loader.construct_scalar(node)
    elif isinstance(node,yaml.SequenceNode):value=loader.construct_sequence(node)
    else:value=loader.construct_mapping(node)
    if tag=='GetAtt' and isinstance(value,str):value=value.split('.',1)
    return {tag if tag=='Ref' else 'Fn::'+tag:value}
Loader.add_multi_constructor('!',intrinsic)
p=argparse.ArgumentParser();p.add_argument('--image-uri',required=True);p.add_argument('--output',type=Path,required=True);a=p.parse_args()
root=Path(__file__).resolve().parent.parent
data=yaml.load((root/'infra/template.yaml').read_text(),Loader=Loader)
for resource in data['Resources'].values():
    if resource['Type']=='AWS::Serverless::Function':
        resource.pop('Metadata',None);resource['Properties']['ImageUri']=a.image_uri
    elif resource['Type']=='AWS::PricingPlanManager::Subscription':resource.pop('Metadata',None)
a.output.parent.mkdir(parents=True,exist_ok=True);a.output.write_text(json.dumps(data,indent=2))
print(a.output)
