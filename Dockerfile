FROM rust:alpine AS builder
WORKDIR /code/fusen-net
COPY . .
RUN apk add --no-cache -U musl-dev
RUN RUSTFLAGS="-C target-feature=-crt-static" cargo build --release
 

FROM alpine
WORKDIR /opt/fusen-net
COPY --from=builder /code/fusen-net/target/release/server .
RUN apk add --no-cache -U libgcc
EXPOSE 8089
ENTRYPOINT ["./server"]