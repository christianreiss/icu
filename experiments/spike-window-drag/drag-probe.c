// drag-probe <title-prefix> [steps] [step_ms] [out-file]
// Drags the first visible top-level window whose title starts with
// <title-prefix> by its title bar with synthetic mouse input, while a
// sampler thread polls the window's position every ~1 ms. Reports how
// closely the window followed the pointer: position updates per second,
// the longest stretch without one while the pointer moved, and the lag
// (pointer travel minus window travel) in pixels.
#include <windows.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static const char *prefix;
static HWND target;
static volatile LONG running = 1;
static volatile LONG cursor_dx; // pointer travel so far, px
#define MAXS 200000
static double s_t[MAXS];
static LONG s_wx[MAXS], s_cx[MAXS];
static volatile LONG ns;
static LARGE_INTEGER freq, t0;

static double now_ms(void) {
    LARGE_INTEGER t; QueryPerformanceCounter(&t);
    return (double)(t.QuadPart - t0.QuadPart) * 1000.0 / (double)freq.QuadPart;
}

static BOOL CALLBACK find(HWND h, LPARAM l) {
    char title[256];
    if (!IsWindowVisible(h)) return TRUE;
    GetWindowTextA(h, title, sizeof title);
    if (strncmp(title, prefix, strlen(prefix)) == 0) { target = h; return FALSE; }
    return TRUE;
}

static DWORD WINAPI sampler(LPVOID p) {
    RECT r0; GetWindowRect(target, &r0);
    while (running && ns < MAXS) {
        RECT r; GetWindowRect(target, &r);
        LONG i = ns;
        s_t[i] = now_ms(); s_wx[i] = r.left - r0.left; s_cx[i] = cursor_dx;
        ns = i + 1;
        Sleep(1);
    }
    return 0;
}

static void mouse(DWORD flags, LONG x, LONG y) {
    INPUT in = {0};
    in.type = INPUT_MOUSE;
    in.mi.dx = (LONG)((x * 65535.0) / (GetSystemMetrics(SM_CXSCREEN) - 1));
    in.mi.dy = (LONG)((y * 65535.0) / (GetSystemMetrics(SM_CYSCREEN) - 1));
    in.mi.dwFlags = flags | MOUSEEVENTF_ABSOLUTE;
    SendInput(1, &in, sizeof in);
}

static int cmp(const void *a, const void *b) {
    double x = *(const double *)a, y = *(const double *)b;
    return x < y ? -1 : x > y;
}

int main(int argc, char **argv) {
    if (argc < 2) { fprintf(stderr, "usage: drag-probe <title-prefix> [steps] [step_ms] [out]\n"); return 2; }
    prefix = argv[1];
    int steps = argc > 2 ? atoi(argv[2]) : 150;
    int step_ms = argc > 3 ? atoi(argv[3]) : 8;
    FILE *out = argc > 4 ? fopen(argv[4], "w") : stdout;
    if (!out) return 3;
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    timeBeginPeriod(1);
    QueryPerformanceFrequency(&freq); QueryPerformanceCounter(&t0);
    EnumWindows(find, 0);
    if (!target) { fprintf(out, "no window titled %s*\n", prefix); return 1; }
    SetForegroundWindow(target);
    Sleep(300);
    RECT r; GetWindowRect(target, &r);
    // Title bar: the middle, a few pixels below the top frame.
    LONG x = r.left + (r.right - r.left) / 2, y = r.top + 12;
    const int dx = 3;
    mouse(MOUSEEVENTF_MOVE, x - 1, y);
    Sleep(50);
    mouse(MOUSEEVENTF_MOVE, x, y);
    Sleep(50);
    mouse(MOUSEEVENTF_LEFTDOWN, x, y);
    Sleep(100);
    HANDLE th = CreateThread(NULL, 0, sampler, NULL, 0, NULL);
    double start = now_ms();
    for (int i = 1; i <= steps; i++) {
        // Back and forth, so the window stays on screen.
        int k = i <= steps / 2 ? i : steps - i;
        cursor_dx = k * dx;
        mouse(MOUSEEVENTF_MOVE, x + k * dx, y);
        double due = start + (double)i * step_ms;
        while (now_ms() < due) Sleep(1);
    }
    Sleep(300);
    mouse(MOUSEEVENTF_LEFTUP, x, y);
    Sleep(100);
    running = 0;
    WaitForSingleObject(th, 2000);
    double end = start + (double)steps * step_ms;

    // Window position changes while the pointer was moving.
    int changes = 0; double last = -1, max_gap = 0, max_gap_at = 0;
    int long_gaps = 0;
    double *gaps = malloc(sizeof(double) * (ns + 1)); int ng = 0;
    double lag_sum = 0; LONG lag_max = 0; int lag_n = 0;
    double *lags = malloc(sizeof(double) * (ns + 1));
    for (LONG i = 1; i < ns; i++) {
        if (s_t[i] < start || s_t[i] > end) continue;
        if (s_wx[i] != s_wx[i - 1]) {
            if (last >= 0) { double g = s_t[i] - last; gaps[ng++] = g; if (g > 40) long_gaps++; if (g > max_gap) { max_gap = g; max_gap_at = last - start; } }
            last = s_t[i]; changes++;
        }
        LONG lag = labs(s_cx[i] - s_wx[i]);
        lags[lag_n++] = lag; lag_sum += lag; if (lag > lag_max) lag_max = lag;
    }
    qsort(gaps, ng, sizeof(double), cmp);
    qsort(lags, lag_n, sizeof(double), cmp);
    double secs = (end - start) / 1000.0;
    fprintf(out, "window=%s samples=%ld input_rate=%.0f/s\n", prefix, ns, 1000.0 / step_ms);
    fprintf(out, "updates=%.1f/s gap_p50=%.1fms gap_p95=%.1fms gap_max=%.1fms (at %.0fms) gaps_over_40ms=%d\n",
            changes / secs, ng ? gaps[ng / 2] : -1, ng ? gaps[ng * 95 / 100] : -1, max_gap, max_gap_at, long_gaps);
    fprintf(out, "lag_mean=%.1fpx lag_p95=%.0fpx lag_max=%ldpx\n",
            lag_n ? lag_sum / lag_n : -1, lag_n ? lags[lag_n * 95 / 100] : -1, lag_max);
    if (out != stdout) fclose(out);
    return 0;
}
