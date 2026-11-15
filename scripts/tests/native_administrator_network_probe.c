/* Small static service for the disposable administrator network qualification. */
#include <netinet/in.h>
#include <signal.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

static volatile sig_atomic_t stopped;

static void stop(int signal) {
  (void)signal;
  stopped = 1;
}

int main(int argc, char **argv) {
  struct sigaction action = {0};
  action.sa_handler = stop;
  sigaction(SIGTERM, &action, 0);

  int udp = argc > 1 && strcmp(argv[1], "udp") == 0;
  int once = argc > 1 && strcmp(argv[1], "once") == 0;
  int listener = socket(AF_INET, udp ? SOCK_DGRAM : SOCK_STREAM, 0);
  int reuse = 1;
  setsockopt(listener, SOL_SOCKET, SO_REUSEADDR, &reuse, sizeof(reuse));
  struct sockaddr_in address = {
      .sin_family = AF_INET,
      .sin_port = htons(udp ? 26380 : 26379),
      .sin_addr = {.s_addr = 0},
  };
  if (bind(listener, (void *)&address, sizeof(address)) ||
      (!udp && listen(listener, 8))) {
    return 2;
  }

  while (!stopped) {
    if (udp) {
      char bytes[64];
      struct sockaddr_in peer;
      socklen_t size = sizeof(peer);
      ssize_t length = recvfrom(listener, bytes, sizeof(bytes), 0,
                                (void *)&peer, &size);
      if (length >= 0) {
        sendto(listener, bytes, (size_t)length, 0, (void *)&peer, size);
      }
    } else {
      int client = accept(listener, 0, 0);
      if (client < 0) {
        continue;
      }
      const char *reply = "HTTP/1.1 200 OK\r\nContent-Length: 7\r\n"
                          "Connection: close\r\n\r\nmanaged";
      (void)write(client, reply, strlen(reply));
      close(client);
      if (once) {
        break;
      }
    }
  }
  close(listener);
  return 0;
}
