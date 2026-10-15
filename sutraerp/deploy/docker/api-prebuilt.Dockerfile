# Part 1 demo: the runtime stage of api.Dockerfile around a sutra-api binary built on the host
# (`cargo build -p sutra-api`, copied to deploy/docker/demo-bin/sutra-api by the demo workflow).
# Used only by docker-compose.demo.yml; production images keep the two-stage api.Dockerfile.
FROM gcr.io/distroless/cc-debian13:nonroot
ARG SUTRA_GIT_SHA=unknown
LABEL org.opencontainers.image.source="https://github.com/Varad1614/ERP_hark_2" \
      org.opencontainers.image.title="sutraerp-api-demo" \
      org.opencontainers.image.revision="${SUTRA_GIT_SHA}"
COPY deploy/docker/demo-bin/sutra-api /usr/local/bin/sutra-api
ENV SUTRA_ENV=production SUTRA_HTTP_ADDR=0.0.0.0:8080 SUTRA_LOG_FORMAT=json SUTRA_GIT_SHA=${SUTRA_GIT_SHA}
EXPOSE 8080
USER nonroot
ENTRYPOINT ["/usr/local/bin/sutra-api"]
CMD ["serve"]
