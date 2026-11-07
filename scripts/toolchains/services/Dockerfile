ARG BASE=melissa-services-base:1b4ea48f329d
FROM ${BASE}
USER root
COPY wheels /tmp/service-wheels
COPY requirements.lock /tmp/service-requirements.lock
RUN /usr/bin/python3 -m venv /opt/melissa-contracts && /opt/melissa-contracts/bin/python -m pip install --no-index --find-links=/tmp/service-wheels --require-hashes -r /tmp/service-requirements.lock && /opt/melissa-contracts/bin/python -m pip check && /opt/melissa-contracts/bin/schemathesis --version && rm -rf /tmp/service-wheels /tmp/service-requirements.lock
