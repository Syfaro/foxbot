FROM rust:1.48-slim-buster AS builder
WORKDIR /src
COPY ./foxbot ./foxbot
RUN strip ./foxbot

FROM debian:buster-slim
ENV HTTP_HOST=127.0.0.1:8080 METRICS_HOST=127.0.0.1:8081
EXPOSE 8080 8081
WORKDIR /app
COPY ./langs ./langs
COPY ./templates ./templates
COPY ./migrations ./migrations
COPY --from=builder /src/foxbot /bin/foxbot
RUN apt-get update -y && apt-get install libssl-dev ca-certificates python3 python3-pip nodejs ffmpeg -y && pip3 install cfscrape && apt-get clean && rm -rf ~/.cache/pip/*
CMD ["/bin/foxbot"]
