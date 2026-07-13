// SPDX-License-Identifier: GPL-2.0-or-later
/*
 * motu424-ctl - the Linux "CueMix FX": a userspace management/control app for
 * the MOTU PCI-324/424 ALSA driver (motu424).
 *
 * MOTU's CueMix FX is a no-latency monitor mixer + clock/format control that
 * runs on the card's DSP. On Linux the driver exposes that control set as ALSA
 * kcontrols (see docs/cuemix-control-map.md); this tool manages them through
 * alsa-lib - the same way alsamixer/amixer manage any card. It auto-locates the
 * MOTU card (driver "motu424"), and can list/get/set controls or print a
 * CueMix-style status overview.
 *
 *   motu424-ctl                       # status overview of the MOTU card
 *   motu424-ctl list                  # every kcontrol (name / type / value)
 *   motu424-ctl get 'Clock Source'
 *   motu424-ctl set 'Clock Source' Internal
 *   motu424-ctl set 'Mix 00 Input 03 Volume' 92
 *   motu424-ctl -D hw:1 list          # target a specific control device
 *
 * Build:  cc -O2 -Wall -o motu424-ctl motu424-ctl.c -lasound
 *
 * The driver's mixer is card-gated (Phase 5); with no MOTU card, or a card that
 * only exposes PCM so far, the tool says so and lists whatever is present.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <alsa/asoundlib.h>

#define DRIVER_MATCH "motu424"
#define NAME_MATCH   "MOTU"

/* ------------------------------------------------------------------ card find */

/*
 * Fill ctlname ("hw:N") for the first card whose driver or name matches MOTU.
 * Returns 0 on success, -1 if no MOTU card is present.
 */
static int find_motu_card(char *ctlname, size_t len)
{
	int card = -1;
	snd_ctl_card_info_t *info;
	snd_ctl_card_info_alloca(&info);

	while (snd_card_next(&card) == 0 && card >= 0) {
		char hw[32];
		snd_ctl_t *ctl;

		snprintf(hw, sizeof(hw), "hw:%d", card);
		if (snd_ctl_open(&ctl, hw, 0) < 0)
			continue;
		if (snd_ctl_card_info(ctl, info) == 0) {
			const char *drv = snd_ctl_card_info_get_driver(info);
			const char *nm  = snd_ctl_card_info_get_name(info);

			if ((drv && strcasecmp(drv, DRIVER_MATCH) == 0) ||
			    (nm && strcasestr(nm, NAME_MATCH))) {
				snprintf(ctlname, len, "hw:%d", card);
				snd_ctl_close(ctl);
				return 0;
			}
		}
		snd_ctl_close(ctl);
	}
	return -1;
}

/* --------------------------------------------------------------- value output */

static const char *type_name(snd_ctl_elem_type_t t)
{
	switch (t) {
	case SND_CTL_ELEM_TYPE_BOOLEAN:    return "bool";
	case SND_CTL_ELEM_TYPE_INTEGER:    return "int";
	case SND_CTL_ELEM_TYPE_INTEGER64:  return "int64";
	case SND_CTL_ELEM_TYPE_ENUMERATED: return "enum";
	case SND_CTL_ELEM_TYPE_BYTES:      return "bytes";
	default:                           return "?";
	}
}

/* Print one element's current value(s) to stdout (no newline). */
static void print_value(snd_hctl_elem_t *elem, snd_ctl_elem_info_t *info)
{
	snd_ctl_elem_value_t *val;
	snd_ctl_elem_type_t type = snd_ctl_elem_info_get_type(info);
	unsigned int n = snd_ctl_elem_info_get_count(info), i;

	snd_ctl_elem_value_alloca(&val);
	if (snd_hctl_elem_read(elem, val) < 0) {
		printf("<read error>");
		return;
	}

	for (i = 0; i < n; i++) {
		if (i)
			putchar(',');
		switch (type) {
		case SND_CTL_ELEM_TYPE_BOOLEAN:
			printf("%s", snd_ctl_elem_value_get_boolean(val, i) ?
			       "on" : "off");
			break;
		case SND_CTL_ELEM_TYPE_INTEGER:
			printf("%ld", snd_ctl_elem_value_get_integer(val, i));
			break;
		case SND_CTL_ELEM_TYPE_INTEGER64:
			printf("%lld",
			       snd_ctl_elem_value_get_integer64(val, i));
			break;
		case SND_CTL_ELEM_TYPE_ENUMERATED: {
			unsigned int idx =
				snd_ctl_elem_value_get_enumerated(val, i);
			snd_ctl_elem_info_set_item(info, idx);
			if (snd_hctl_elem_info(elem, info) == 0)
				printf("%s",
				       snd_ctl_elem_info_get_item_name(info));
			else
				printf("%u", idx);
			break;
		}
		default:
			printf("<%s>", type_name(type));
		}
	}
}

/* Print the range/enum items an element accepts, in brackets, to stream f. */
static void print_range(FILE *f, snd_hctl_elem_t *elem,
			snd_ctl_elem_info_t *info)
{
	snd_ctl_elem_type_t type = snd_ctl_elem_info_get_type(info);

	if (type == SND_CTL_ELEM_TYPE_INTEGER) {
		fprintf(f, " [%ld..%ld]", snd_ctl_elem_info_get_min(info),
			snd_ctl_elem_info_get_max(info));
	} else if (type == SND_CTL_ELEM_TYPE_ENUMERATED) {
		unsigned int items = snd_ctl_elem_info_get_items(info), i;

		fprintf(f, " {");
		for (i = 0; i < items; i++) {
			snd_ctl_elem_info_set_item(info, i);
			if (snd_hctl_elem_info(elem, info) < 0)
				continue;
			fprintf(f, "%s%s", i ? "|" : "",
				snd_ctl_elem_info_get_item_name(info));
		}
		fprintf(f, "}");
	}
}

/* ------------------------------------------------------------------- commands */

static snd_hctl_elem_t *find_by_name(snd_hctl_t *hctl, const char *name)
{
	snd_ctl_elem_id_t *id;
	snd_ctl_elem_id_alloca(&id);
	snd_ctl_elem_id_set_interface(id, SND_CTL_ELEM_IFACE_MIXER);
	snd_ctl_elem_id_set_name(id, name);
	return snd_hctl_find_elem(hctl, id);
}

static int cmd_list(snd_hctl_t *hctl)
{
	snd_hctl_elem_t *elem;
	snd_ctl_elem_info_t *info;
	int count = 0;

	snd_ctl_elem_info_alloca(&info);
	for (elem = snd_hctl_first_elem(hctl); elem;
	     elem = snd_hctl_elem_next(elem)) {
		if (snd_hctl_elem_info(elem, info) < 0)
			continue;
		printf("%-32s %-6s = ", snd_hctl_elem_get_name(elem),
		       type_name(snd_ctl_elem_info_get_type(info)));
		print_value(elem, info);
		print_range(stdout, elem, info);
		putchar('\n');
		count++;
	}
	if (!count)
		printf("(card exposes no controls yet)\n");
	return 0;
}

static int cmd_get(snd_hctl_t *hctl, const char *name)
{
	snd_hctl_elem_t *elem = find_by_name(hctl, name);
	snd_ctl_elem_info_t *info;

	if (!elem) {
		fprintf(stderr, "no such control: %s\n", name);
		return 1;
	}
	snd_ctl_elem_info_alloca(&info);
	if (snd_hctl_elem_info(elem, info) < 0)
		return 1;
	printf("%s = ", name);
	print_value(elem, info);
	print_range(stdout, elem, info);
	putchar('\n');
	return 0;
}

/* Resolve an enum item name (or numeric index) to its index, or -1. */
static int enum_index(snd_hctl_elem_t *elem, snd_ctl_elem_info_t *info,
		      const char *s)
{
	unsigned int items = snd_ctl_elem_info_get_items(info), i;
	char *end;
	long n = strtol(s, &end, 10);

	if (*end == '\0' && n >= 0 && (unsigned long)n < items)
		return (int)n;
	for (i = 0; i < items; i++) {
		snd_ctl_elem_info_set_item(info, i);
		if (snd_hctl_elem_info(elem, info) < 0)
			continue;
		if (strcasecmp(s, snd_ctl_elem_info_get_item_name(info)) == 0)
			return (int)i;
	}
	return -1;
}

static int parse_bool(const char *s)
{
	if (!strcasecmp(s, "on") || !strcasecmp(s, "1") ||
	    !strcasecmp(s, "true") || !strcasecmp(s, "yes"))
		return 1;
	if (!strcasecmp(s, "off") || !strcasecmp(s, "0") ||
	    !strcasecmp(s, "false") || !strcasecmp(s, "no"))
		return 0;
	return -1;
}

static int cmd_set(snd_hctl_t *hctl, const char *name, int argc, char **argv)
{
	snd_hctl_elem_t *elem = find_by_name(hctl, name);
	snd_ctl_elem_info_t *info;
	snd_ctl_elem_value_t *val;
	snd_ctl_elem_type_t type;
	unsigned int n, i;

	if (!elem) {
		fprintf(stderr, "no such control: %s\n", name);
		return 1;
	}
	snd_ctl_elem_info_alloca(&info);
	snd_ctl_elem_value_alloca(&val);
	if (snd_hctl_elem_info(elem, info) < 0)
		return 1;
	type = snd_ctl_elem_info_get_type(info);
	n = snd_ctl_elem_info_get_count(info);
	if (snd_hctl_elem_read(elem, val) < 0)
		return 1;

	/* one value applies to all members; N values set them individually */
	for (i = 0; i < n; i++) {
		const char *s = (argc == 1) ? argv[0] :
				(i < (unsigned)argc ? argv[i] : NULL);
		if (!s)
			break;
		switch (type) {
		case SND_CTL_ELEM_TYPE_BOOLEAN: {
			int b = parse_bool(s);
			if (b < 0) {
				fprintf(stderr, "expected on/off: %s\n", s);
				return 1;
			}
			snd_ctl_elem_value_set_boolean(val, i, b);
			break;
		}
		case SND_CTL_ELEM_TYPE_INTEGER:
			snd_ctl_elem_value_set_integer(val, i, strtol(s, NULL, 0));
			break;
		case SND_CTL_ELEM_TYPE_ENUMERATED: {
			int idx = enum_index(elem, info, s);
			if (idx < 0) {
				fprintf(stderr, "invalid value '%s' for %s",
					s, name);
				print_range(stderr, elem, info);
				fputc('\n', stderr);
				return 1;
			}
			snd_ctl_elem_value_set_enumerated(val, i, idx);
			break;
		}
		default:
			fprintf(stderr, "cannot set control of type %s\n",
				type_name(type));
			return 1;
		}
	}
	if (snd_hctl_elem_write(elem, val) < 0) {
		fprintf(stderr, "write failed for %s\n", name);
		return 1;
	}
	return cmd_get(hctl, name);
}

/* CueMix-style overview: clock/format up top, then a count of the matrix. */
static int cmd_status(snd_hctl_t *hctl, const char *ctlname)
{
	snd_hctl_elem_t *elem;
	snd_ctl_elem_info_t *info;
	int inputs = 0, mixes = 0, sends = 0, outputs = 0, total = 0;

	printf("MOTU card on %s\n", ctlname);

	if ((elem = find_by_name(hctl, "Clock Source"))) {
		snd_ctl_elem_info_alloca(&info);
		if (snd_hctl_elem_info(elem, info) == 0) {
			printf("  Clock Source : ");
			print_value(elem, info);
			putchar('\n');
		}
	}
	if ((elem = find_by_name(hctl, "Clock Rate"))) {
		snd_ctl_elem_info_alloca(&info);
		if (snd_hctl_elem_info(elem, info) == 0) {
			printf("  Clock Rate   : ");
			print_value(elem, info);
			printf(" Hz\n");
		}
	}

	snd_ctl_elem_info_alloca(&info);
	for (elem = snd_hctl_first_elem(hctl); elem;
	     elem = snd_hctl_elem_next(elem)) {
		const char *nm = snd_hctl_elem_get_name(elem);

		total++;
		if (strncmp(nm, "Input ", 6) == 0 && strstr(nm, "Trim"))
			inputs++;
		else if (strncmp(nm, "Mix ", 4) == 0 && strstr(nm, "Master"))
			mixes++;
		else if (strncmp(nm, "Mix ", 4) == 0 && strstr(nm, "Volume"))
			sends++;
		else if (strncmp(nm, "Output ", 7) == 0 && strstr(nm, "Volume"))
			outputs++;
	}
	printf("  Mixer        : %d input(s), %d mix bus(es), %d matrix send(s)\n",
	       inputs, mixes, sends);
	if (outputs) {
		printf("  Outputs      : %d output(s)", outputs);
		if ((elem = find_by_name(hctl, "Patchbay Switch"))) {
			snd_ctl_elem_info_alloca(&info);
			if (snd_hctl_elem_info(elem, info) == 0) {
				printf(", patchbay ");
				print_value(elem, info);
			}
		}
		putchar('\n');
	}
	printf("  Controls     : %d total   (use 'list' to see them all)\n",
	       total);
	if (!total)
		printf("  note: driver exposes no mixer controls yet "
		       "(Phase 5, needs a card).\n");
	return 0;
}

/* ----------------------------------------------------------------------- main */

static void usage(void)
{
	fprintf(stderr,
		"usage: motu424-ctl [-D <ctldev>] [command]\n"
		"  (no command)            CueMix-style status overview\n"
		"  list                    list every kcontrol\n"
		"  get  <name>             read one control\n"
		"  set  <name> <val...>    write one control\n"
		"  -D hw:N                 target a specific control device\n");
}

int main(int argc, char **argv)
{
	char ctlname[64] = "";
	snd_hctl_t *hctl;
	int rc, argi = 1;
	const char *cmd;

	if (argc > 2 && strcmp(argv[1], "-D") == 0) {
		snprintf(ctlname, sizeof(ctlname), "%s", argv[2]);
		argi = 3;
	} else if (find_motu_card(ctlname, sizeof(ctlname)) < 0) {
		fprintf(stderr,
			"no MOTU card found (driver '%s'). Load the module, or "
			"pass -D hw:N.\n", DRIVER_MATCH);
		return 1;
	}

	if ((rc = snd_hctl_open(&hctl, ctlname, 0)) < 0) {
		fprintf(stderr, "open %s: %s\n", ctlname, snd_strerror(rc));
		return 1;
	}
	if ((rc = snd_hctl_load(hctl)) < 0) {
		fprintf(stderr, "load %s: %s\n", ctlname, snd_strerror(rc));
		snd_hctl_close(hctl);
		return 1;
	}

	cmd = (argi < argc) ? argv[argi] : "status";
	if (!strcmp(cmd, "status")) {
		rc = cmd_status(hctl, ctlname);
	} else if (!strcmp(cmd, "list")) {
		rc = cmd_list(hctl);
	} else if (!strcmp(cmd, "get") && argi + 1 < argc) {
		rc = cmd_get(hctl, argv[argi + 1]);
	} else if (!strcmp(cmd, "set") && argi + 2 < argc) {
		rc = cmd_set(hctl, argv[argi + 1], argc - (argi + 2),
			     &argv[argi + 2]);
	} else {
		usage();
		rc = 2;
	}

	snd_hctl_close(hctl);
	return rc;
}
