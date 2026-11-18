/*
 * thp_off — лончер uwuwu-server с выключенным THP (срез A #1294).
 *
 * Railway memory-метрика = cgroup memory.current; THP=always на хостах
 * раздувает cgroup-учёт (прецедент TrailBase, A/B: -34%, идентичные
 * контейнеры). prctl(PR_SET_THP_DISABLE) переживает execve и действует
 * на всё дерево потомков — лончер делает prctl и exec'ает CMD образа.
 *
 * Сборка — в Dockerfile (build-stage, cc от rust:slim + gcc);
 * бинарь динамический под общий debian:bookworm-slim runtime-stage.
 * Откат — revert одного коммита (автодеплой вернёт CMD без лончера).
 */
#define _GNU_SOURCE
#include <stdio.h>
#include <sys/prctl.h>
#include <unistd.h>

#ifndef PR_SET_THP_DISABLE
#define PR_SET_THP_DISABLE 41
#endif

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: thp_off CMD [ARGS...]\n");
        return 2;
    }
    if (prctl(PR_SET_THP_DISABLE, 1, 0, 0, 0) != 0) {
        perror("thp_off: prctl(PR_SET_THP_DISABLE)");
    }
    execvp(argv[1], &argv[1]);
    perror("thp_off: execvp");
    return 1;
}
