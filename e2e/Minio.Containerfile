# syntax=docker/dockerfile:1
# The upstream container registries and binary archive are no longer public.
# Keep the E2E S3 server on an immutable upstream MinIO release instead.
FROM golang:1.25.13-bookworm AS builder
ADD --checksum=sha256:8b11129f6a6830768bcdebab569290ad956618f5ff6ef4857b633acb64f95126 \
    https://codeload.github.com/minio/minio/tar.gz/01ce918d8279a20e4706b96a64396146894adee4 /tmp/minio.tar.gz
RUN mkdir /src && tar -xzf /tmp/minio.tar.gz --strip-components=1 -C /src
WORKDIR /src
RUN --mount=type=cache,target=/go/pkg/mod \
    --mount=type=cache,target=/root/.cache/go-build \
    CGO_ENABLED=0 GOTOOLCHAIN=local go build -trimpath -o /out/minio .

FROM alpine:3.22.2
COPY --from=builder /out/minio /usr/local/bin/minio
COPY --from=builder /src/LICENSE /licenses/minio/LICENSE
ENTRYPOINT ["minio"]
