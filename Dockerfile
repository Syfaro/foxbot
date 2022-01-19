FROM ubuntu:20.04
ENV HTTP_HOST=127.0.0.1:8080
EXPOSE 8080
WORKDIR /app
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY ./langs ./langs
COPY ./foxbot /bin/foxbot
CMD ["/bin/foxbot"]
