FROM rustlang/rust:nightly-slim
ENV HTTP_HOST=127.0.0.1:8080
EXPOSE 8080
COPY . .
RUN apt-get -y update && apt-get -y install pkg-config libssl-dev
RUN cargo install --root / --path .
CMD ["/bin/foxbot"]
