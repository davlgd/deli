#!/usr/bin/env bash
# Installed by the ISO build: builds use every processor of the machine
# they run on, not the build host's count.
echo "*/* build_options: jobs=$(nproc)"
