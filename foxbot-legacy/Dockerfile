FROM ubuntu:20.04
ENV HTTP_HOST=127.0.0.1:8080 METRICS_HOST=127.0.0.1:8081
EXPOSE 8080 8081
WORKDIR /app
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY ./langs ./langs
COPY ./templates ./templates
COPY ./foxbot/foxbot /bin/foxbot
CMD ["/bin/foxbot"]
