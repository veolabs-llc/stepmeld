# hello-program as an image, for the walkthrough's fleet run as
# containers (README.md). Built from the top of the repository:
#   docker build -f examples/hello/Dockerfile -t stepmeld-hello:1 .
# The program inside speaks the run directory protocol as it does on a
# machine: stepmeld mounts the Run's folder and appends its path.
FROM rust:1.97.1-slim AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p stepmeld-example-hello

FROM debian:bookworm-slim
COPY --from=build /src/target/release/hello-program /usr/local/bin/hello-program
ENTRYPOINT ["hello-program"]
