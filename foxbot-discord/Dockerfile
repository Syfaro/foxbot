FROM rust:1.50-slim-buster AS builder
WORKDIR /src
COPY ./foxbot-discord/foxbot-discord ./foxbot-discord
RUN strip ./foxbot-discord

FROM debian:buster-slim
RUN apt-get update -y && apt-get install libssl-dev ca-certificates -y && apt-get clean
COPY --from=builder /src/foxbot-discord /bin/foxbot-discord
CMD ["/bin/foxbot-discord"]
