FROM debian:buster-slim
RUN apt-get update -y && apt-get install libssl-dev ca-certificates python3 python3-pip nodejs ffmpeg -y && pip3 install cfscrape && apt-get clean && rm -rf ~/.cache/pip/*
