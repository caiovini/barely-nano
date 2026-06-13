#include <config.h>
#include <common.h>
#include <dm.h>
#include <dm/device.h>
#include <dm/device-internal.h>
#include <dm/uclass-internal.h>
#include <net.h>
#include <linux/types.h>
#include <api_public.h>

extern void eth_set_dev(struct udevice *dev);

#define DEBUG
#undef DEBUG

#ifdef DEBUG
#define debugf(fmt, args...) do { printf("%s(): ", __func__); printf(fmt, ##args); } while (0)
#else
#define debugf(fmt, args...)
#endif

#define errf(fmt, args...) do { printf("ERROR @ %s(): ", __func__); printf(fmt, ##args); } while (0)

#if defined(CONFIG_CMD_NET)

static int dev_valid_net(void *cookie)
{
	struct uclass *uc;
	struct udevice *it;

	if (uclass_get(UCLASS_ETH, &uc))
		return 0;

	uclass_foreach_dev(it, uc) {
		if (it == (struct udevice *)cookie)
			return 1;
	}
	return 0;
}

int dev_open_net(void *cookie)
{
	struct udevice *dev = (struct udevice *)cookie;
	printf("api_net: dev_open_net(cookie=%p)\n", cookie);
	if (!dev_valid_net(cookie)) {
		printf("api_net: cookie is INVALID\n");
		return API_ENODEV;
	}

	printf("api_net: probing device %s...\n", dev->name);
	if (device_probe(dev)) {
		printf("api_net: probe FAILED\n");
		return API_EIO;
	}

	eth_set_dev(dev);
	if (eth_init() < 0) {
		printf("api_net: eth_init FAILED\n");
		return API_EIO;
	}

	printf("api_net: dev_open_net SUCCESS\n");
	return 0;
}

int dev_close_net(void *cookie)
{
	struct udevice *dev = (struct udevice *)cookie;
	if (!dev)
		return API_ENODEV;

	eth_halt();
	device_remove(dev, DM_REMOVE_NORMAL);
	return 0;
}

int dev_enum_net(struct device_info *di)
{
	struct udevice *dev = NULL;
	struct eth_pdata *pdata;

	if (di->cookie == NULL) {
		/* Get first device in ETH uclass */
		uclass_first_device_check(UCLASS_ETH, &dev);
	} else {
		/* Get next device after the one in cookie */
		dev = (struct udevice *)di->cookie;
		uclass_next_device_check(&dev);
	}

	if (!dev)
		return 0;

	di->cookie = dev;
	di->type = DEV_TYP_NET;
	
	pdata = dev_get_plat(dev);
	if (pdata)
		memcpy(di->di_net.hwaddr, pdata->enetaddr, 6);

	debugf("device found: %s, cookie 0x%p\n", dev->name, di->cookie);

	return 1;
}

int dev_write_net(void *cookie, void *buf, int len)
{
	return eth_get_ops((struct udevice *)cookie)->send((struct udevice *)cookie, buf, len);
}

int dev_read_net(void *cookie, void *buf, int len)
{
	uchar *packet;
	int rv;
	struct udevice *dev = (struct udevice *)cookie;

	rv = eth_get_ops(dev)->recv(dev, 0, &packet);
	if (rv > 0) {
		if (rv > len) rv = len;
		memcpy(buf, packet, rv);
		if (eth_get_ops(dev)->free_pkt)
			eth_get_ops(dev)->free_pkt(dev, packet, rv);
	}

	return rv;
}

#else

int dev_open_net(void *cookie)
{
	return API_ENODEV;
}

int dev_close_net(void *cookie)
{
	return API_ENODEV;
}

int dev_enum_net(struct device_info *di)
{
	return 0;
}

int dev_write_net(void *cookie, void *buf, int len)
{
	return API_ENODEV;
}

int dev_read_net(void *cookie, void *buf, int len)
{
	return API_ENODEV;
}

#endif
