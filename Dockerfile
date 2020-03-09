FROM rustlang/rust:nightly-slim AS builder
WORKDIR /src
RUN apt-get update -y && apt-get install pkg-config libssl-dev python3 python3-dev -y
ARG DRONE_COMMIT_SHA
ENV RELEASE $DRONE_COMMIT_SHA
COPY . .
RUN cargo install --root / --path .

FROM debian:buster-slim
ENV HTTP_HOST=127.0.0.1:8080
EXPOSE 8080
WORKDIR /app
COPY --from=builder /src/langs ./langs
COPY --from=builder /bin/foxbot /bin/foxbot
RUN apt-get update -y && apt-get install libssl-dev ca-certificates python3 python3-pip nodejs -y && pip3 install cfscrape
CMD ["/bin/foxbot"]
