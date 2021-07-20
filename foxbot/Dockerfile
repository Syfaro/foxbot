FROM rust:1.53-slim-buster AS builder
WORKDIR /src
COPY ./foxbot/foxbot ./foxbot
RUN strip ./foxbot

FROM registry.huefox.com/syfaro/foxbot/base
ENV HTTP_HOST=127.0.0.1:8080 METRICS_HOST=127.0.0.1:8081
EXPOSE 8080 8081
WORKDIR /app
COPY ./langs ./langs
COPY ./templates ./templates
COPY ./migrations ./migrations
COPY --from=builder /src/foxbot /bin/foxbot
CMD ["/bin/foxbot"]
