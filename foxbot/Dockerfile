FROM registry.huefox.com/project/foxbot/base
ENV HTTP_HOST=127.0.0.1:8080 METRICS_HOST=127.0.0.1:8081
EXPOSE 8080 8081
WORKDIR /app
COPY ./langs ./langs
COPY ./templates ./templates
COPY ./foxbot/foxbot /bin/foxbot
CMD ["/bin/foxbot"]
