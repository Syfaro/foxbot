FROM rustlang/rust:nightly-slim AS builder
RUN strip ./foxbot

FROM debian:buster-slim
ENV HTTP_HOST=127.0.0.1:8080
EXPOSE 8080
WORKDIR /app
COPY ./langs ./langs
COPY ./templates ./templates
COPY ./foxbot /bin/foxbot
RUN apt-get update -y && apt-get install libssl-dev ca-certificates python3 python3-pip nodejs ffmpeg -y && pip3 install cfscrape && apt-get clean && rm -rf ~/.cache/pip/*
CMD ["/bin/foxbot"]
