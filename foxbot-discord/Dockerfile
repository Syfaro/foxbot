FROM ubuntu:20.04
WORKDIR /app
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY ./foxbot-discord/foxbot-discord /bin/foxbot-discord
CMD ["/bin/foxbot-discord"]
