FROM public.ecr.aws/lambda/python:3.12
COPY requirements.txt ${LAMBDA_TASK_ROOT}/requirements.txt
RUN pip install --no-cache-dir -r ${LAMBDA_TASK_ROOT}/requirements.txt --target ${LAMBDA_TASK_ROOT}
COPY newspaper ${LAMBDA_TASK_ROOT}/newspaper
COPY config ${LAMBDA_TASK_ROOT}/config
COPY web ${LAMBDA_TASK_ROOT}/web
CMD ["newspaper.handlers.collector"]
