"""Explicit post-approval operation; never sync the private biographical profile."""
import argparse,json
import boto3
from newspaper.pipeline import config
p=argparse.ArgumentParser();p.add_argument('--bucket',required=True);p.add_argument('--approved-deployment',action='store_true');a=p.parse_args()
if not a.approved_deployment:p.error('Run only after explicit AWS deployment approval')
data=config('interests')
assert set(data)<= {'version','last_reviewed','topics','reading_level'}
boto3.client('s3').put_object(Bucket=a.bucket,Key='config/interests.json',Body=json.dumps(data).encode(),ContentType='application/json',ServerSideEncryption='AES256')
print('Uploaded sanitized interest configuration')
