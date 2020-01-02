FROM rustlang/rust:nightly-alpine AS builder
WORKDIR /src
ENV RUSTFLAGS="-C target-feature=-crt-static"
RUN apk add build-base libressl-dev
COPY . .
RUN cargo install --root / --path .

FROM alpine
ENV HTTP_HOST=127.0.0.1:8080
EXPOSE 8080
WORKDIR /app
COPY --from=builder /src/langs ./langs
COPY --from=builder /bin/foxbot /bin/foxbot
RUN apk add build-base libressl-dev
CMD ["/bin/foxbot"]
