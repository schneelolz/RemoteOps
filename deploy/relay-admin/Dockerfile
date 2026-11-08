# 固定官方基础镜像摘要，前端镜像只复制已验证的静态构建产物。
FROM nginx:stable-alpine@sha256:e9b2d7624fd7957f4ed44ae0a272d93fb73aeb8512ba8916f8118ae6143fd115
COPY deploy/relay-admin/nginx.conf /etc/nginx/nginx.conf
COPY relay-admin/dist/ /usr/share/nginx/html/
USER 101:101
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 CMD wget -q -O - http://127.0.0.1:8080/healthz || exit 1
ENTRYPOINT ["nginx"]
CMD ["-g", "daemon off;"]
