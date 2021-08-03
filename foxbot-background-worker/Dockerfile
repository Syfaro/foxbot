FROM registry.huefox.com/project/foxbot/base
WORKDIR /app
COPY ./langs ./langs
COPY ./foxbot-background-worker/foxbot-background-worker /bin/foxbot-background-worker
CMD ["/bin/foxbot-background-worker"]
