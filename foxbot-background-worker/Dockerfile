FROM ubuntu:20.04
WORKDIR /app
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY ./langs ./langs
COPY ./foxbot-background-worker/foxbot-background-worker /bin/foxbot-background-worker
CMD ["/bin/foxbot-background-worker"]
