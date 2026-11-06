ARG BASE=melissa-code06-complete:20261020
FROM ${BASE}
USER root
COPY nextest.tar.gz llvm-cov.tar.gz mutants.tar.gz /tmp/verification-tools/
RUN cd /tmp/verification-tools && echo '1d4ce302b820c41f4b89c5fb9bce45f0f0c94882c1cf08fc360caf08be6e7df4  nextest.tar.gz' | sha256sum -c - && echo 'b3f68e625481fed9b16444174f3fa5ebcdbde4a1878803a35eabe2dcefcdc41a  llvm-cov.tar.gz' | sha256sum -c - && echo 'dfe6dc37d0342c891d2829b5a695aa57c2d0edecef7e7d0399a30cc6e206411e  mutants.tar.gz' | sha256sum -c - && tar -xzf nextest.tar.gz -C /usr/local/cargo/bin && tar -xzf llvm-cov.tar.gz -C /usr/local/cargo/bin && tar -xzf mutants.tar.gz -C /usr/local/cargo/bin && /usr/local/cargo/bin/rustup component add llvm-tools-preview --toolchain 1.98.1 && rm -rf /tmp/verification-tools
RUN /usr/local/cargo/bin/cargo nextest --version && /usr/local/cargo/bin/cargo llvm-cov --version && /usr/local/cargo/bin/cargo mutants --version
