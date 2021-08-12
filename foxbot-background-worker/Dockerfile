FROM debian:slim
WORKDIR /app
COPY ./langs ./langs
COPY ./foxbot-background-worker/foxbot-background-worker /bin/foxbot-background-worker
CMD ["/bin/foxbot-background-worker"]
